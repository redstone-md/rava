//! The compile request: what gets compiled, with what plans, and what is refused.

use crate::vm::jit_plans::CompilePlans;
use crate::*;

/// Mirrors of `Vm::jit_compiled` / `jit_cap` for the periodic profile dump (see
/// `[JIT-COVERAGE]`): the census counters are per-Vm and the dumper has no `&Vm`.
pub(crate) static COMPILED_N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub(crate) static CAP_N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Keep the compile-only contract synchronous except for one explicitly authorized dispatch body.
#[inline]
fn compile_only_can_queue(key: usize) -> bool {
    !COMPILE_ONLY.with(|compile_only| compile_only.get())
        || COMPILE_ASYNC_ONLY_KEY.with(|async_key| async_key.get() == key)
}


// ═══════════════════════════════════════════════════════════════════════════════════════════════
//                    THE VM ENTRY GATE WORD — one acquire load for the whole prologue
// ═══════════════════════════════════════════════════════════════════════════════════════════════

/// `RAVA_JIT_DENY` names at least one class substring or one exact `Class#method`. Clear ⇒ both
/// `jit_deny_class` and `jit_deny_method` are provably `false` (an empty `Vec::any` and an
/// `is_empty()` set), so neither is called at all.
#[allow(dead_code)] // the deny tables only exist under `next-gen-gc`
const VG_DENY: u8 = 1 << 0;
/// `callcensus::enabled()` — `RAVA_CALL_CENSUS=<substr>` is set.
const VG_CALLCENSUS: u8 = 1 << 2;
/// `worker_hot_enabled() || wide_tier_enabled()` — a worker visit past the threshold leaves the
/// owning thread a note. TWO acquire loads at that site, one bit here.
const VG_WORKER_NOTE: u8 = 1 << 3;
/// `wide_tier_enabled()` — the per-class native-served refusal may narrow, and may record its
/// verdict in `jit_cache`. Read TWICE inside `native_served_compile_refusal`, one bit here.
#[allow(dead_code)]
const VG_WIDE_TIER: u8 = 1 << 4;
/// KILL SWITCH `RAVA_NO_VM_GATEWORD=1` (or the blanket `RAVA_NO_EMIT_MORE=1`): every folded
/// constant is read back at its own call site from its own static, and the compile-only field-id
/// pre-resolution goes back onto the per-entry path — exactly the shape this round found. The
/// word is still built, because it has to be to carry this bit; what the switch restores is the
/// READS, not the ANSWERS, which never differed.
const VG_LEGACY: u8 = 1 << 5;
/// KILL SWITCH `RAVA_NO_FAST_JITRUN2=1` (or the blanket `RAVA_NO_EMIT_MORE=1`): restores this
/// round's three per-ENTRY shapes exactly — the second `jit_cache` probe, the unconditional
/// `native_served_compile_refusal` call for methods whose verdict is already in the cache, and
/// the two process-global atomic read-modify-writes that count every passage. It rides the gate
/// word that is already read once at the top of the prologue, so asking for it costs no load:
/// a switch that needed its own `OnceLock` would put back exactly the acquire barrier this file
/// spent a round removing.
const VG_SLOW_RUN: u8 = 1 << 6;
/// KILL SWITCH `RAVA_NO_FAST_JITENTER=1`: keep already-compiled entries on the full compile
/// prologue, including the background-result poll. The default path enters the immutable
/// compiled verdict directly; a pending method still reaches the poll and publishes itself.
const VG_SLOW_ENTER: u8 = 1 << 7;

/// ⭐**ONE READ FOR THE WHOLE `jit_run` PROLOGUE — the `natives::entry::entry_gates()` shape,
/// applied to the engine's hottest entry.**
///
/// A product boot makes **~21.7 M passages through `jit_run`** (14.1 M interpreted entries +
/// 7.6 M compiled ones — the population `native_served_compile_refusal` already documents), and
/// every single one of them used to answer four separate process-constant questions from four
/// separate `OnceLock`s:
///
/// | site (per ENTRY)                        | what it asked                              |
/// |-----------------------------------------|--------------------------------------------|
/// | `callcensus::enabled()`                 | is `RAVA_CALL_CENSUS` set                  |
/// | `jit_deny_class()` → `deny_tables()`    | is `RAVA_JIT_DENY` set                     |
/// | `jit_deny_method()` → `deny_tables()`   | is `RAVA_JIT_DENY` set (**again**)         |
/// | `dalist_get_inline_enabled()`           | is `RAVA_NO_DALIST_GET_INLINE` unset       |
///
/// A `OnceLock` read is an ACQUIRE-ORDERED atomic load. It is not a free `mov`: LLVM may not
/// hoist it out of the caller's loop, may not forward one across the opaque calls that sit
/// between these sites (`drain_background_jit`, `resolve_field_id`), and must materialise the
/// surrounding values around it. Four statics therefore meant four loads, four branches and four
/// scheduling barriers, per method entry, to learn that no diagnostic is armed.
///
/// **Removed per entry: 4 acquire loads → 1**, plus the five RELAXED loads of the inline-arm
/// field-id cells (`DALIST_A_FID`, `DALIST_SIZE_FID`, `CUBE_PARTS_FID`, `ENTRY_KEY_FID`,
/// `ENTRY_VAL_FID`) which are COMPILE-decision work and have moved to the compile decision — see
/// `Vm::prewarm_inline_field_ids`. Nine atomic loads per entry become one.
///
/// Two more sites off the per-entry path but on the same word, because they were the same defect:
/// the worker hand-off read `worker_hot_enabled()` **and** `wide_tier_enabled()` (2 acquire loads
/// on 482,902 worker visits a boot), and `native_served_compile_refusal` read
/// `wide_tier_enabled()` twice in one call.
///
/// ⚠**Position is preserved, not just cost.** Each remaining bit is the SAME predicate read from
/// the SAME accessor; the deny-list refusal still precedes the per-class refusal. Spline
/// compilation is admitted separately, only when this Code has a structural HOF plan.
#[inline]
pub(crate) fn vm_gates() -> u8 {
    static W: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *W.get_or_init(|| {
        let mut w = 0u8;
        // Evaluated in the order the per-entry sites used to evaluate them, so the one gate with
        // a side effect (`callcensus::enabled` arms its watchdog) still fires first and still
        // fires on the first `jit_run` passage, exactly as before.
        if crate::callcensus::enabled() {
            w |= VG_CALLCENSUS;
        }
        #[cfg(feature = "next-gen-gc")]
        {
            let (subs, exact) = crate::deny_tables();
            if !subs.is_empty() || !exact.is_empty() {
                w |= VG_DENY;
            }
        }
        if crate::vm::worker_hot_enabled() || wide_tier_enabled() {
            w |= VG_WORKER_NOTE;
        }
        if wide_tier_enabled() {
            w |= VG_WIDE_TIER;
        }
        // Falls together with the other emit-more switches, as every gate in this tree does.
        if std::env::var_os("RAVA_NO_VM_GATEWORD").is_some() || crate::flags::emit_more_disabled() {
            w |= VG_LEGACY;
        }
        if std::env::var_os("RAVA_NO_FAST_JITRUN2").is_some() || crate::flags::emit_more_disabled()
        {
            w |= VG_SLOW_RUN;
        }
        if std::env::var_os("RAVA_NO_FAST_JITENTER").is_some()
            || crate::flags::emit_more_disabled()
        {
            w |= VG_SLOW_ENTER;
        }
        w
    })
}

// ═══════════════════════════════════════════════════════════════════════════════════════════════
//        THE TWO PASSAGE CENSUSES — ONE THREAD-LOCAL INCREMENT INSTEAD OF TWO SHARED `lock xadd`
// ═══════════════════════════════════════════════════════════════════════════════════════════════
//
// ⭐**TWO PROCESS-GLOBAL ATOMIC READ-MODIFY-WRITES PER METHOD ENTRY, COUNTING THE SAME THING.**
// `JIT_GATE[3]` ("every ENTRY into the compile decision") is bumped at the top of `jit_run` and
// `compile_bail::SEEN` ("jit_run passages") a few lines below it; a product boot makes ~22 M
// passages (15.2 M interpreted entries + 7.7 M compiled ones, `compile_bail`'s own numbers), so
// that is ~44 M `lock xadd` on two shared cache lines.
//
// A relaxed `fetch_add` is NOT a `mov`. On x86 it is a locked read-modify-write that must take
// the line EXCLUSIVE, and both of these lines are written by every thread that runs guest code —
// `jit_run` is entered by workers too (see `JIT_GATE[5]`, 482,902 worker visits a boot past the
// threshold alone). During the datapack phase, which is 44% of the boot and the only part that
// is really parallel, those two lines ping-pong between cores on every single method entry. And
// the second cost is the one this tree already priced at −15% on the interpreter's push path:
// an atomic is an OPTIMISATION BARRIER, so nothing around it may be hoisted or reordered.
//
// Neither counter is read by any DECISION — `diag/mod.rs`, `lane_probe.rs`, `pc_profile` and
// `[CBAIL]` only print them — so the population may be counted per thread and published in
// blocks. Per entry this leaves one TLS load, one add, one compare and one store.
//
// ⚠**WHAT CHANGES, STATED PLAINLY:** a reader sees a value that lags by up to `ENTRY_FLUSH - 1`
// per counter per live thread, and a thread that exits mid-block never publishes its remainder.
// On a population of 22 M with ~10 threads that is a worst case of ~0.05%, well under the
// resolution any of these censuses is quoted at. `RAVA_NO_FAST_JITRUN2=1` puts both atomics back
// on the per-entry path, exactly as they were.
//
// ⚠**AND THE TWO POPULATIONS ARE KEPT SEPARATE**, because they are not identical: `JIT_GATE[3]`
// counts the passage before early `native_claimed` and deny-list refusals; `SEEN` counts after
// them. Structural spline fallback is now decided only at a cold compile attempt, after both.

/// Passages tallied per thread before one flush to each process counter.
const ENTRY_FLUSH: u32 = 1024;

thread_local! {
    /// LOW 32 bits: passages not yet added to `JIT_GATE[3]`.
    /// HIGH 32 bits: passages not yet added to `crate::jit::compile_bail::SEEN`.
    ///
    /// `const`-initialised and destructor-free, which is what makes an access ONE TLS load —
    /// a lazily-initialised or `Drop`-carrying thread-local costs a guard branch and a dtor-list
    /// registration on top (rule 1 of the gate-folding vein).
    static ENTRY_TALLY: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Out of line so the atomic never appears in `jit_run`'s own instruction stream.
#[cold]
#[inline(never)]
fn flush_gate3() {
    JIT_GATE[3].fetch_add(ENTRY_FLUSH as usize, std::sync::atomic::Ordering::Relaxed);
}

#[cold]
#[inline(never)]
fn flush_seen() {
    crate::jit::compile_bail::SEEN
        .fetch_add(ENTRY_FLUSH as u64, std::sync::atomic::Ordering::Relaxed);
}

/// `JIT_GATE[3]`: every entry into the compile decision, counted before any refusal.
#[inline]
fn note_entry(gw: u8) {
    if gw & VG_SLOW_RUN != 0 {
        JIT_GATE[3].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        return;
    }
    let full = ENTRY_TALLY.with(|c| {
        let v = c.get();
        let low = (v as u32).wrapping_add(1);
        let full = low >= ENTRY_FLUSH;
        c.set((v & !0xffff_ffffu64) | if full { 0 } else { low as u64 });
        full
    });
    if full {
        flush_gate3();
    }
}

/// `compile_bail::SEEN`: passages that survived the diagnostic refusals, i.e. the population the
/// `[CBAIL]` line calls "jit_run passages". Same tally cell, high half.
#[inline]
fn note_cbail_seen(gw: u8) {
    if gw & VG_SLOW_RUN != 0 {
        crate::jit::compile_bail::note_seen();
        return;
    }
    let full = ENTRY_TALLY.with(|c| {
        let v = c.get();
        let high = ((v >> 32) as u32).wrapping_add(1);
        let full = high >= ENTRY_FLUSH;
        c.set((v & 0xffff_ffffu64) | if full { 0 } else { (high as u64) << 32 });
        full
    });
    if full {
        flush_seen();
    }
}

/// The `jit_cache` verdict for one method, copied out so no borrow of the cache is held across
/// the compile/insert calls below. `Cold(0)` doubles as "no entry", which is what every reader of
/// this state has always treated an absent entry as.
enum CacheState {
    Compiled(*const u8, Option<jit::JT>, bool, u32),
    Compiling,
    Failed,
    Cold(u32),
}

/// Immutable metadata published beside a `Code` after its `jit_cache` entry becomes `Compiled`.
/// The executable allocation already has process lifetime; this record is leaked for the same
/// bounded method population so readers on worker threads need only one acquire load.
struct PublishedJitEntry {
    ptr: usize,
    ret: Option<jit::JT>,
    self_reg: bool,
    mid: u32,
}

#[inline]
fn published_jit_entry(code: &Code) -> Option<CacheState> {
    use std::sync::atomic::Ordering::Acquire;
    let p = code.jit_entry_hint.load(Acquire);
    if p == 0 {
        return None;
    }
    // SAFETY: publication stores a leaked `PublishedJitEntry` with release ordering. Deopt clears
    // the pointer but never frees either this record or the executable code it names.
    let e = unsafe { &*(p as *const PublishedJitEntry) };
    Some(CacheState::Compiled(
        e.ptr as *const u8,
        e.ret,
        e.self_reg,
        e.mid,
    ))
}

#[inline(never)]
pub(crate) fn publish_jit_entry_hint(
    code_key: usize,
    ptr: *const u8,
    ret: Option<jit::JT>,
    self_reg: bool,
    mid: u32,
) {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if !*ENABLED.get_or_init(|| {
        std::env::var_os("RAVA_NO_JIT_ENTRY_HINT").is_none()
            && !crate::flags::emit_more_disabled()
    }) {
        return;
    }
    let entry = Box::into_raw(Box::new(PublishedJitEntry {
        ptr: ptr as usize,
        ret,
        self_reg,
        mid,
    })) as usize;
    // SAFETY: every `code_key` originates from a live `&Code`; loaded classes and their method
    // bodies outlive the VM's JIT cache. Publication happens only after the cache owns Compiled.
    let code = unsafe { &*(code_key as *const Code) };
    code.jit_entry_hint
        .store(entry, std::sync::atomic::Ordering::Release);
}

#[inline]
pub(crate) fn clear_jit_entry_hint(code_key: usize) {
    // SAFETY: deopt receives the same stable Code identity embedded by the compile driver.
    let code = unsafe { &*(code_key as *const Code) };
    code.jit_entry_hint
        .store(0, std::sync::atomic::Ordering::Release);
}

impl Vm {
    /// methods include the receiver as arg 0).
    /// JIT fast path: count this method's invocations and, once hot, compile it
    /// (numeric methods only — `jit::compile` returns None otherwise, recorded
    /// as Failed so we never retry). When a compiled function exists, run it on
    /// the packed locals and return its result. `None` => interpret as usual.
    pub(crate) fn jit_run(
        &mut self,
        code: &Code,
        name: &str,
        descriptor: &str,
        is_static: bool,
        class: &Arc<LoadedClass>,
        locals: &[Value],
    ) -> Option<Result<Option<Value>, VmError>> {
        // Only the owning VM thread may publish a background compiler result:
        // `jit_cache` is VM state, while a gil-free worker must keep interpreting
        // until the main thread has made the entry visible.
        // ⭐**FOUR PROCESS CONSTANTS, ONE ACQUIRE LOAD** — see `vm_gates`. Read once here, at the
        // top of the prologue, and threaded down to `native_served_compile_refusal` and the
        // worker hand-off rather than re-read there. `native_claimed::armed()` below is
        // deliberately NOT in the word: it is a RUNTIME latch (`native_claimed::arm()` is called
        // from `native_backing.rs` while the process runs), not a process constant, so folding it
        // into a `OnceLock` would freeze an answer that is allowed to change.
        let gw = vm_gates();
        // [3] = every ENTRY into the compile decision, [4] = entries that found a Cold entry and
        // bumped its counter. Tallied per thread and published in blocks of `ENTRY_FLUSH` — the
        // counter itself is unchanged, only the `lock xadd` that used to publish it on all ~22 M
        // passages. See the census block above. The gate word is read first because this call
        // consults it; `vm_gates()`'s one first-passage side effect (arming the call census) is
        // unaffected by being one statement earlier, and it still fires on the first passage.
        note_entry(gw);
        // ⭐**A METHOD THE NATIVES HAVE ANSWERED IS NEVER COMPILED** (see `native_claimed`).
        //
        // The namespace exclusion is per-NAMESPACE and native coverage is per-METHOD, which is
        // why relaxing it compiled bodies that then served receivers the natives were handling.
        // No predicate in the tree answers "would the natives serve this method" — the cascade
        // executes an arm rather than answering — so the answer is taken from what the cascade
        // ACTUALLY did in this process. Inert unless the relaxation armed the table.
        if crate::native_claimed::armed()
            && crate::native_claimed::claimed(&class.file.this_class, name, descriptor)
        {
            return None;
        }
        // One bit test instead of the `OnceLock` behind `callcensus::enabled()`. Same predicate,
        // same accessor, same arm; under the kill switch the accessor is called again.
        let census = if gw & VG_LEGACY != 0 {
            crate::callcensus::enabled()
        } else {
            gw & VG_CALLCENSUS != 0
        };
        if census {
            crate::callcensus::note(&class.file.this_class, name);
        }
        // An append-only `Compiled` verdict needs no compile-side maintenance. Polling the
        // background result channel here cannot help this call; if a pending method is invoked,
        // its own `Compiling` verdict falls through and drains the queue before dispatch.
        // Diagnostic deny/JIT-stat modes retain the old path and its ordering.
        let key = code as *const Code as usize;
        let fast_state = gw & (VG_LEGACY | VG_SLOW_RUN) == 0;
        let fast_enter = gw & (VG_LEGACY | VG_SLOW_RUN | VG_SLOW_ENTER | VG_DENY) == 0
            && !self.trace.jit_stats;
        if fast_enter {
            if let Some(CacheState::Compiled(ptr, ret, self_reg, mid)) =
                published_jit_entry(code)
            {
                note_cbail_seen(gw);
                return crate::vm::ExecKernel::new(self).enter_jit(
                    ptr,
                    ret,
                    self_reg,
                    mid,
                    code,
                    class,
                    name,
                    descriptor,
                    is_static,
                    locals,
                    None,
                );
            }
        }
        let probed = if fast_state {
            Some(self.jit_cache_state(key))
        } else {
            None
        };
        if fast_enter {
            if let Some(CacheState::Compiled(ptr, ret, self_reg, mid)) = &probed {
                note_cbail_seen(gw);
                return crate::vm::ExecKernel::new(self).enter_jit(
                    *ptr,
                    *ret,
                    *self_reg,
                    *mid,
                    code,
                    class,
                    name,
                    descriptor,
                    is_static,
                    locals,
                    None,
                );
            }
        }
        if !gil::is_worker() {
            self.drain_background_jit();
            // Same place, same thread: serve the compile requests workers could not serve
            // themselves. See `Vm::note_worker_hot` — 482,902 worker visits a boot land on a
            // method already past the threshold, and none of them ever compiled.
            self.drain_worker_hot();
        }
        // DIAGNOSTIC (RAVA_JIT_DENY=substr): never compile/run a method whose class name
        // contains the substring — it stays interpreted. Used to deopt a suspect family
        // (e.g. com/mojang/datafixers) and test whether a wall is a compiled-codegen bug.
        //
        // ⭐**TWO ACQUIRE LOADS OF THE SAME `OnceLock`, PER ENTRY, TO ASK WHETHER A DIAGNOSTIC
        // ENV VAR IS SET.** `jit_deny_class` and `jit_deny_method` each call `deny_tables()`, and
        // with `RAVA_JIT_DENY` unset both then answer `false` from an empty `Vec` and an empty
        // `HashSet`. `VG_DENY` is exactly "either table is non-empty", so the unset case — every
        // production run — reaches neither call. Set, both run in their original order.
        #[cfg(feature = "next-gen-gc")]
        if gw & (VG_LEGACY | VG_DENY) != 0
            && (jit_deny_class(&class.file.this_class)
                || jit_deny_method(&class.file.this_class, name))
        {
            JIT_GATE[0].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return None;
        }
        // A NATIVE-SERVED class (guava immutables, native collections, caches) keeps
        // its real state in synthetic fields the native layer manages — its BYTECODE
        // reads never-populated real fields (null/garbage). The interpreter's dispatch
        // gauntlet serves such calls natively so the bytecode normally never runs, but
        // any path that DOES invoke it (an interface default, a super call, an exotic
        // dispatch) must interpret, never compile: a compiled body would run the broken
        // bytecode with no native fallback (same class as dd13a9d for inline/dispatch).
        //
        // ⭐**AND IT IS THE LARGEST INTERPRETED POPULATION IN THE ENGINE.** This arm returned with
        // NO COUNTER, which is why `jit.denied_native_served` read 0 while this refusal was firing
        // constantly — that gauge is wired to a different check. `RAVA_INTERP_WHO` on
        // `Bootstrap.bootStrap` names the victims and they are all here:
        //
        //   150,917 x2  DoubleArrayList.equals        (fastutil)
        //    64,177     AbstractIndexedListIterator.hasNext  (guava)
        //    48,218     Lists.newArrayList            (guava)
        //    31,510     AbstractIndexedListIterator.next
        //     ~8,000 each  LinkedTreeMap.find/put/rebalance/rotateLeft  (gson)
        //
        // `DoubleArrayList.equals` alone is **24% of every interpreted entry in the phase**, and
        // the top ten refusals are 39% of it. ⚠Note what that implies: the comment above says the
        // native layer serves these calls "so the bytecode normally never runs" — but 301,834
        // interpreted entries into one method say the bytecode runs constantly and produces
        // CORRECT results (the gates would fail otherwise). So for at least this class the
        // blanket per-CLASS refusal is broader than its own justification.
        //
        // Narrowing it is not a matter of listing method names — production eligibility may not
        // be routed by name — it needs BYTECODE evidence that a body does not read a field the
        // native layer shadows. `RAVA_JIT_NS_BYTECODE` is the existing attempt at that and it
        // CRASHES the boot (0xC0000005 in `datapack_prewarm_tokenize`), so the counter comes
        // first: an unmeasured refusal cannot be narrowed safely.
        note_cbail_seen(gw);
        // The `Code` pointer is this method's identity and keys everything below; computing it
        // HERE costs one pointer cast and is what lets the per-CLASS refusal be asked once per
        // METHOD instead of once per ENTRY. See `native_served_compile_refusal`.
        // ⭐**ONE `jit_cache` PROBE PER ENTRY, WHERE THERE WERE TWO.** Both the per-class refusal
        // and the state machine below ask the same map the same question with the same key on
        // every one of ~22 M passages: the refusal opens with "is this method already decided?"
        // and the state machine then re-reads the entry it just looked at. `jit_cache` holds
        // ~15 k entries — hundreds of kilobytes, so a probe is an L2-class access, not a free
        // one — and the two probes are separated by nothing that can write it:
        //
        //   * `native_served_compile_refusal` writes ONLY on the arm that returns `true`, and
        //     that arm returns from `jit_run` immediately;
        //   * `wide_tier_admits` and `jit_native_served_deep` take `&self`;
        //   * `jit_stats_tick` takes `&self`; `ensure_field_hints` / `ensure_static_cells` touch
        //     the heap; `jit_stats` is a different map.
        //
        // So the second probe is provably the first probe's answer. Taken once, here, and the
        // refusal is handed the verdict instead of re-deriving it. Under `RAVA_NO_VM_GATEWORD`
        // the legacy `prewarm_inline_field_ids` call sits BETWEEN the two positions and takes
        // `&mut self`, so that switch keeps the old two-probe shape rather than asking anyone to
        // audit it; `RAVA_NO_FAST_JITRUN2` does the same.
        // ⭐**AND THE REFUSAL IS NOT CALLED AT ALL FOR A METHOD ALREADY DECIDED.** `Compiled`,
        // `Compiling` and `Failed` are precisely the states it answers "do not refuse" for — and
        // they are the overwhelming majority of passages, since a refused method is recorded as
        // `Failed` and a compiled one stays `Compiled` for the life of the process. The call, its
        // frame and its probe now happen only for the `None`/`Cold` entries the test actually
        // decides. Same verdict and order against the deny-list and per-class refusal.
        #[cfg(feature = "next-gen-gc")]
        {
            // `Cold(n)` also encodes "no entry", which is exactly the set the refusal decides.
            // With no probe in hand (the kill-switch arms) the callee makes its own, as today.
            let ask = match &probed {
                Some(s) => matches!(s, CacheState::Cold(_)),
                None => true,
            };
            if ask && self.native_served_compile_refusal(gw, key, class, probed.is_none()) {
                return None;
            }
        }
        // ⭐**COMPILE-DECISION WORK, TAKEN OFF THE RUN DECISION.** The inline-arm field-id
        // pre-resolution below used to run HERE, on all ~21.7 M entries — one acquire load
        // (`dalist_get_inline_enabled`) plus FIVE relaxed loads of the id cells, forever, for a
        // job whose whole contract is "be resolved before this method is COMPILED". It now runs
        // at the point the state machine has just proved a compile is about to happen (~18 k
        // passages a boot, i.e. ~1,200x fewer), which is strictly no later relative to any
        // compile: the resolution sits immediately above `jit_plans` and the emitter, and the
        // background path resolves before it submits. Under `RAVA_NO_VM_GATEWORD` it runs here
        // again, unchanged.
        #[cfg(feature = "next-gen-gc")]
        if gw & VG_LEGACY != 0 {
            self.prewarm_inline_field_ids();
        }
        // Periodic per-method table on a wall clock, so a boot killed at Done still reports.
        // `jit_stats_tick`'s own first statement is this same plain-bool test, hoisted to the
        // call site: off (every production run) the passage now reads one `Vm` field instead of
        // making a call that reads it. The callee still tests it, so this is a cost move only.
        #[cfg(feature = "next-gen-gc")]
        if self.trace.jit_stats {
            self.jit_stats_tick();
        }
        // Publish the field-hint + getstatic-cell table bases before any compiled
        // method can run.
        self.heap.ensure_field_hints();
        self.heap.ensure_static_cells();
        let stat_key = self
            .trace
            .jit_stats
            .then(|| format!("{}.{}{}", class.file.this_class, name, descriptor));
        if let Some(k) = stat_key.as_deref() {
            self.jit_stats.entry(k.to_string()).or_default().seen += 1;
        }
        // Snapshot the cache entry (copying out the raw pointer) so no borrow is
        // held across compile()/insert(). Taken above the refusal on the fast path, and only
        // re-derived here under the kill switches — see the probe comment there.
        use CacheState as S;
        let s = match probed {
            Some(s) => s,
            None => self.jit_cache_state(key),
        };
        let (ptr, ret, self_reg, mid) = match s {
            S::Compiling => return None,
            S::Failed => {
                if let Some(k) = stat_key.as_deref() {
                    self.jit_stats
                        .entry(k.to_string())
                        .or_default()
                        .cached_failed += 1;
                }
                return None;
            }
            S::Cold(n) => {
                JIT_GATE[4].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                // Both of these are COMPILE-decision inputs — the warm-up threshold and the
                // class identity the emitter bakes — and both used to be materialised in the
                // prologue, i.e. on the ~7.7 M passages that find a `Compiled` entry and the
                // interpreted passages that find a `Failed` one, neither of which can reach a
                // compile. Read where they are used.
                let threshold = self.jit_threshold;
                let class_ptr = Arc::as_ptr(class) as i64;
                let n2 = n + 1;
                match n2 {
                    64 => { COLD_LEVEL[0].fetch_add(1, std::sync::atomic::Ordering::Relaxed); }
                    128 => { COLD_LEVEL[1].fetch_add(1, std::sync::atomic::Ordering::Relaxed); }
                    256 => { COLD_LEVEL[2].fetch_add(1, std::sync::atomic::Ordering::Relaxed); }
                    512 => { COLD_LEVEL[3].fetch_add(1, std::sync::atomic::Ordering::Relaxed); }
                    _ => {}
                }
                // Load-time structural bits are the sole eager-eligibility authority. Runtime
                // feature switches may disable a preclassified execution strategy, but names,
                // counts, timings, and prior outcomes cannot create eligibility here.
                let compile_eager_now =
                    crate::structural_loop_eager::compile_eager_now(code.structural_flags);
                let first_entry = first_entry_compile_enabled();
                // A worker (daemon OS) thread never compiles — its JITModule would
                // be freed on thread exit, dangling shared jit_cache pointers. It
                // keeps counting so the MAIN thread compiles the method later. A
                // cold-parallel proof primes the count to make that next main-thread
                // invocation compile without repeating the structural analysis.
                if gil::is_worker() {
                    // [5] = every worker visit, [6] = a worker visit that had ALREADY crossed the
                    // threshold. A worker never compiles (its JITModule dies with the thread), so
                    // [6] counts methods proven hot whose compilation simply never happens unless
                    // the MAIN thread happens to call them again.
                    JIT_GATE[5].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if first_entry || n2 >= threshold {
                        JIT_GATE[6].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        // Leave a note for the owning thread, which CAN compile. Three owned
                        // strings, no pointers, no heap access — legal off-GIL.
                        //
                        // ⭐**AND THE NOTE IS NOW KEYED BY `Code` POINTER, WHICH IS WHY IT CAN BE
                        // DEFAULT-ON.** This fires on 482,902 worker visits a boot and the old
                        // shape built three `String`s and CLONED them — six allocations per visit,
                        // under a global mutex — to answer "have I already reported this method?"
                        // for a set of ~1,000. The identity was in hand all along: `key`, the same
                        // `Code` pointer `jit_cache` uses. Names are now materialised only for a
                        // method reported for the FIRST time.
                        //
                        // TWO acquire loads became one bit test, on the 482,902 worker visits a
                        // boot that reach this arm: both operands are process constants folded
                        // into `VG_WORKER_NOTE`, and the kill switch reads them at their own
                        // statics again.
                        let note = if gw & VG_LEGACY != 0 {
                            crate::vm::worker_hot_enabled() || wide_tier_enabled()
                        } else {
                            gw & VG_WORKER_NOTE != 0
                        };
                        if note {
                            Self::note_worker_hot(key, &class.file.this_class, name, descriptor);
                        }
                    }
                    if let Some(k) = stat_key.as_deref() {
                        self.jit_stats.entry(k.to_string()).or_default().cold += 1;
                    }
                    self.jit_cache.insert(key, jit::JitStatus::Cold(n2));
                    return None;
                }
                if !first_entry && !compile_eager_now && (n == 0 || n2 < threshold) {
                    if let Some(k) = stat_key.as_deref() {
                        self.jit_stats.entry(k.to_string()).or_default().cold += 1;
                    }
                    self.jit_cache.insert(key, jit::JitStatus::Cold(n2));
                    return None;
                }
                // [7] = PASSED the warm-up gate, i.e. this method is now going to be compiled
                // unless something further down refuses. 1,675 methods cross the 512 threshold
                // (COLD_LEVEL) and only 20 reach the compile attempt, so the loss is between
                // those two points and this counter halves the interval.
                JIT_GATE[7].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                // ⭐**THE INLINE ARMS' FIELD IDS, RESOLVED ON THE COMPILE DECISION.** This is the
                // one point both compile paths pass through and the state machine has just proved
                // a compile is imminent, so the ids are published before `jit_plans`, before the
                // background submit and before the synchronous emitter — the whole of their
                // contract — at ~18 k passages a boot instead of ~21.7 M. One acquire load and
                // five relaxed loads leave the per-entry path; see `prewarm_inline_field_ids`.
                #[cfg(feature = "next-gen-gc")]
                if gw & VG_LEGACY == 0 {
                    self.prewarm_inline_field_ids();
                }
                // Diagnostic only: classify each distinct hot method against the generic
                // callback-free MAP analyzer. This never feeds back into compilation or
                // scheduling; names are emitted solely to locate bytecode for follow-up.
                #[cfg(feature = "next-gen-gc")]
                if jit::pool_scan_enabled() {
                    jit::pool_scan_probe(
                        &class.file.this_class,
                        name,
                        descriptor,
                        &code.bytecode,
                        &class.file.pool,
                    );
                }
                // `jit_hot_probes` lives in `jit_probes`, the AOT diagnostics module, which is
                // windows-x86_64 only because it names backends that consume `cnp`.
                #[cfg(all(windows, target_arch = "x86_64"))]
                self.jit_hot_probes(code, class, name, descriptor);
                // Cap total compiled methods: each owns a JITModule that mmaps
                // executable memory the GC/meter can't reclaim. Past the cap, leave
                // the method interpreted (mark Failed so it isn't retried).
                COMPILED_N.store(self.jit_compiled as u64, std::sync::atomic::Ordering::Relaxed);
                CAP_N.store(self.jit_cap as u64, std::sync::atomic::Ordering::Relaxed);
                if self.jit_compiled >= self.jit_cap {
                    crate::PLANS_NONE[2].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    self.jit_cache.insert(key, jit::JitStatus::Failed);
                    return None;
                }
                // Opt-in tracer bullet: reserve bounded queue capacity before doing plan work.
                // The unset path receives `None` and keeps the same bounded `try_send` scheduling.
                // The permit is RAII-owned, so every planning or field-resolution early return
                // refunds it; after a successful send the worker refunds it immediately on recv.
                let early_queue_permit = if self.background_jit.is_some()
                    && !compile_eager_now
                    && compile_only_can_queue(key)
                {
                    match self
                        .background_jit
                        .as_ref()
                        .expect("background JIT checked above")
                        .try_early_reserve()
                    {
                        Ok(permit) => permit,
                        Err(background_jit::EarlyReserveError::Full) => {
                            crate::PLANS_NONE[0].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            crate::PLANS_NONE[3].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            self.jit_cache.insert(key, jit::JitStatus::Cold(n2));
                            return None;
                        }
                    }
                } else {
                    None
                };
                // The old per-entry pin kept this exact intrinsic interpreted. Preserve that
                // fallback only when structural HOF analysis cannot replace it for this Code.
                // Build once at the actual compile attempt, then pass the same map to jit_plans.
                #[cfg(feature = "next-gen-gc")]
                let prebuilt_hof_fusions = if crate::spline_native_enabled()
                    && name == "findIntervalStart"
                    && class.file.this_class == "net/minecraft/util/CubicSpline$Multipoint"
                {
                    let plans = self.build_hof_fusions(code, class, descriptor, is_static);
                    if plans.is_empty() {
                        JIT_GATE[2].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        self.jit_cache.insert(key, jit::JitStatus::Failed);
                        return None;
                    }
                    Some(plans)
                } else {
                    None
                };
                let pool = &class.file.pool;
                let CompilePlans {
                    static_intrinsics,
                    inline_targets,
                    native_calls,
                    statics,
                    slot_mark,
                    inline_reserved,
                    reserved,
                    #[cfg(feature = "next-gen-gc")]
                    new_arena,
                    #[cfg(feature = "next-gen-gc")]
                    ctor_inlines,
                    #[cfg(feature = "next-gen-gc")]
                    getter_inlines,
                    #[cfg(feature = "next-gen-gc")]
                    concat_shapes,
                    #[cfg(feature = "next-gen-gc")]
                    hof_fusions,
                    #[cfg(feature = "next-gen-gc")]
                    prim_arrays,
                } = match self.jit_plans(
                    code,
                    class,
                    name,
                    descriptor,
                    is_static,
                    #[cfg(feature = "next-gen-gc")]
                    prebuilt_hof_fusions,
                ) {
                    Some(p) => p,
                    None => {
                        // ⭐**COUNT THE SILENT `?`.** This was a bare `?`: planning failed, the
                        // function returned, the cache was NOT marked, and the method passed the
                        // warm-up gate again on its next call — forever. No counter, no trace.
                        crate::PLANS_NONE[0].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        thread_local! {
                            static SEEN: std::cell::RefCell<crate::FastMap<usize, ()>> =
                                std::cell::RefCell::new(crate::FastMap::default());
                        }
                        let first = SEEN.with(|c| c.borrow_mut().insert(key, ()).is_none());
                        if first {
                            crate::PLANS_NONE[1].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            if std::env::var_os("RAVA_TRACE_PLANS").is_some() {
                                eprintln!(
                                    "[PLANS-NONE] {}.{}{}",
                                    class.file.this_class, name, descriptor
                                );
                            }
                        }
                        return None;
                    }
                };
                // A5-PRECURSOR (RAVA_COMPILE_TIME): cumulative wall-time the Cranelift
                // JIT spends COMPILING during the run. This is EXACTLY the time an AOT
                // cache eliminates on a second boot (link-from-disk instead of compile),
                // so it is the ceiling of the A5 boot-time win — measurable safely with
                // zero execution risk (a timer around the existing compile).
                let ct_probe = std::env::var_os("RAVA_COMPILE_TIME").is_some();
                // DIAGNOSTIC (RAVA_TRACE_COMPILE): print every method as it compiles.
                // The LAST few lines before a hang name the miscompile suspects (a
                // spinning compiled loop prints nothing itself — the compile ORDER is
                // the only breadcrumb).
                if std::env::var_os("RAVA_TRACE_COMPILE").is_some() {
                    eprintln!("[COMPILE] {}.{}{}", class.file.this_class, name, descriptor);
                }
                let t0 = if ct_probe { Some(std::time::Instant::now()) } else { None };
                // Deep-IC v3: a non-leaf body whose frame fits the dispatch call
                // buffer gets the self-registering prologue, making it directly
                // callable from the machine-code IC's deep arm.
                #[cfg(feature = "next-gen-gc")]
                let deep_shape = if jit::ic_deep_enabled()
                    && !jit::is_gc_leaf(&code.bytecode)
                    && code.max_locals as usize + code.max_stack as usize
                        <= jit::dispatch_callbuf_slots()
                {
                    Some(jit::DeepShape {
                        args_slots: jit::desc_args_slots(descriptor, is_static),
                        frame_len: code.max_locals + code.max_stack,
                        ctx_slot: self.jit_ctx_slot as *const std::sync::atomic::AtomicUsize
                            as usize,
                    })
                } else {
                    None
                };
                // Devirt armability probe (read-only, gated): tally this method's monomorphic
                // virtual sites + how many have an LLVM-emittable target, on the fast boot path.
                // `scan_devirt_sites` lives in `jit_probes`, which is windows-x86_64 only because
                // it names the AOT backends; the probe is diagnostics and has no other target.
                #[cfg(all(feature = "next-gen-gc", windows, target_arch = "x86_64"))]
                if crate::devirt::enabled() || crate::devirt::steady_enabled() {
                    self.scan_devirt_sites(class, &code.bytecode);
                }
                // M3 9.2c.2a: a method compiling DURING an initCache subtree = a lazy compile the
                // parallel fork must force during serial pre-warm (workers can't compile).
                #[cfg(feature = "next-gen-gc")]
                if crate::m3heap::probe_on() {
                    crate::m3heap::record_compile(&format!("{}.{name}", class.file.this_class));
                }
                // A persistent compiler may overlap its codegen with the interpreter.
                // It receives immutable class data and plans resolved while the VM owns the heap.
                // Compile-only requests remain synchronous by default because the gil-free lane
                // needs an entry before it can enqueue; a dispatch request may opt into the same
                // bounded queue for its exact Code key while retaining the no-execution guard.
                // Structurally eager methods are synchronous too: their first invocation is the
                // work they were admitted to accelerate, so queueing would lose that one-shot loop.
                if self.background_jit.is_some()
                    && !compile_eager_now
                    && compile_only_can_queue(key)
                {
                    let Some(field_ids) = self.jit_background_field_ids(code, pool) else {
                        if reserved + inline_reserved > 0 {
                            self.heap
                                .free_static_slots_from(slot_mark, reserved + inline_reserved);
                        }
                        self.jit_cache.insert(key, jit::JitStatus::Failed);
                        return None;
                    };
                    let job = background_jit::CompileJob {
                        key,
                        class: Arc::clone(class),
                        class_name: class.file.this_class.to_string(),
                        name: name.to_string(),
                        descriptor: descriptor.to_string(),
                        field_ids,
                        static_intrinsics,
                        inline_targets,
                        native_calls,
                        statics,
                        #[cfg(feature = "next-gen-gc")]
                        new_arena,
                        #[cfg(feature = "next-gen-gc")]
                        ctor_inlines,
                        #[cfg(feature = "next-gen-gc")]
                        concat_shapes,
                        #[cfg(feature = "next-gen-gc")]
                        hof_fusions,
                        #[cfg(feature = "next-gen-gc")]
                        prim_arrays,
                        #[cfg(feature = "next-gen-gc")]
                        getter_inlines,
                        #[cfg(feature = "next-gen-gc")]
                        deep: deep_shape,
                        // Computed HERE, on the VM thread: deciding it needs the loader, and
                        // the compiler thread has no `Vm`.
                        #[cfg(feature = "next-gen-gc")]
                        tagged_fields: self.tagged_field_indices(&code.bytecode, &class.file.pool),
                        reserved_static_slots: reserved + inline_reserved,
                        reserved_static_base: slot_mark,
                    };
                    let queued = self
                        .background_jit
                        .as_ref()
                        .expect("background JIT checked above")
                        .try_submit(job, early_queue_permit);
                    match queued {
                        Ok(()) => {
                            self.jit_cache.insert(key, jit::JitStatus::Compiling);
                            return None;
                        }
                        Err(error) => {
                            let (job, disconnected) = match error {
                                background_jit::SubmitError::Full(job) => (job, false),
                                background_jit::SubmitError::Disconnected(job) => (job, true),
                            };
                            if job.reserved_static_slots > 0 {
                                self.heap.free_static_slots_from(
                                    job.reserved_static_base,
                                    job.reserved_static_slots,
                                );
                            }
                            if disconnected {
                                // Preserve the legacy retryable fallback. A transport failure is
                                // not structural evidence that may permanently deny compilation.
                                crate::PLANS_NONE[0]
                                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                self.jit_cache.insert(key, jit::JitStatus::Cold(n2));
                            } else {
                                // A full bounded queue is a soft miss: keep interpreting rather
                                // than waiting behind stale work, then retry on a later entry.
                                crate::PLANS_NONE[0]
                                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                crate::PLANS_NONE[3]
                                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                self.jit_cache.insert(key, jit::JitStatus::Cold(n2));
                            }
                            return None;
                        }
                    }
                }
                // Cranelift codegen runs INSIDE the JIT_EXEC guard, so without this the
                // sampler would bill compile time to whichever compiled frame happens to be
                // on the stack. Park the current-method slot on a synthetic "compiling" id
                // for the duration so codegen shows up as itself. Two relaxed stores per
                // COMPILE (once per method, never per call).
                let _cs = jit::compile_scope();
                let phase = crate::jit_phase_trace::enter("sync-codegen", key);
                let compiled = jit::compile_with_static_intrinsics(
                    code,
                    descriptor,
                    pool,
                    class_ptr,
                    &mut |o, n| {
                        let id = self.resolve_field_id(o, n);
                        let slot = self.layout_slot(o, id);
                        (id, slot)
                    },
                    &static_intrinsics,
                    &inline_targets,
                    &native_calls,
                    &statics,
                    #[cfg(feature = "next-gen-gc")]
                    &new_arena,
                    #[cfg(feature = "next-gen-gc")]
                    &ctor_inlines,
                    #[cfg(feature = "next-gen-gc")]
                    &concat_shapes,
                    #[cfg(feature = "next-gen-gc")]
                    &hof_fusions,
                    #[cfg(feature = "next-gen-gc")]
                    &getter_inlines,
                    #[cfg(feature = "next-gen-gc")]
                    deep_shape,
                    #[cfg(feature = "next-gen-gc")]
                    Some(&self.tagged_field_indices(&code.bytecode, pool)),
                    #[cfg(feature = "next-gen-gc")]
                    &prim_arrays,
                );
                drop(phase);
                if let Some(t0) = t0 {
                    use std::sync::atomic::{AtomicU64, Ordering};
                    static NANOS: AtomicU64 = AtomicU64::new(0);
                    static COUNT: AtomicU64 = AtomicU64::new(0);
                    let ns = t0.elapsed().as_nanos() as u64;
                    let tot = NANOS.fetch_add(ns, Ordering::Relaxed) + ns;
                    let c = COUNT.fetch_add(1, Ordering::Relaxed) + 1;
                    if c % 500 == 0 {
                        eprintln!("[COMPILE-TIME] {} methods compiled, {:.2}s cumulative Cranelift compile", c, tot as f64 / 1e9);
                    }
                }
                if compiled.is_none() && reserved + inline_reserved > 0 {
                    // The inline-callee cells and the top-level cells were reserved
                    // contiguously by this compile (single-threaded path), so one
                    // LIFO free unwinds both.
                    self.heap
                        .free_static_slots_from(slot_mark, reserved + inline_reserved);
                }
                COMPILE_TRIED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if compiled.is_none() {
                    COMPILE_REFUSED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                match compiled {
                    Some(f) => {
                        // Tiny-compile tracer (RAVA_DBG_TINY): list every ≤16-byte method
                        // that compiles, for the tiny-miscompile bisect (deny halves via
                        // the Class#method RAVA_JIT_DENY entries until the culprit pins).
                        if code.bytecode.len() <= 16 && std::env::var_os("RAVA_DBG_TINY").is_some()
                        {
                            eprintln!(
                                "[TINY] {}#{} len={} desc={}",
                                class.file.this_class,
                                name,
                                code.bytecode.len(),
                                descriptor
                            );
                        }
                        if let Some(k) = stat_key.as_deref() {
                            self.jit_stats.entry(k.to_string()).or_default().compile_ok += 1;
                        }
                        crate::compile_fail_census::note(true, &code.bytecode);
                        // Intern the printable identity ONCE, here, where the class/name/
                        // descriptor are in hand — the entry boundary then only publishes a u32.
                        let mut f = f;
                        f.mid = jit::register_compiled_method(
                            &class.file.this_class,
                            name,
                            descriptor,
                        );
                        // PC-sampling registry (RAVA_PROFILE_PC, inert otherwise): the only
                        // point where the finalized code pointer, its length and the interned
                        // id are all in hand.
                        crate::pc_profile::register(
                            f.raw() as usize,
                            f.code_len as usize,
                            f.mid,
                        );
                        let pr = (f.raw(), f.ret, f.self_reg, f.mid);
                        // GAP-S07 invariant: `Compiled` entries are APPEND-ONLY — a pinned
                        // fork `fptr` must never dangle. This site is only reachable from
                        // the Cold/absent states, so a replace here would be a logic bug.
                        debug_assert!(
                            !matches!(self.jit_cache.get(&key), Some(jit::JitStatus::Compiled(_))),
                            "jit_cache must never replace a Compiled entry (GAP-S07 pin)"
                        );
                        Self::debug_assert_not_gilfree("jit_cache (compile publication)");
                        crate::pilot_census::note_compile();
                        self.jit_cache.insert(key, jit::JitStatus::Compiled(f));
                        publish_jit_entry_hint(key, pr.0, pr.1, pr.2, pr.3);
                        self.jit_compiled += 1;
                        // ONE increment. There were two, adjacent and identically
                        // spelled, so every SYNCHRONOUSLY compiled method counted
                        // twice — and `counters.rs` documents this static as the
                        // "global mirror of Vm::jit_compiled", which increments once
                        // right above. The arithmetic is visible in the ledger's own
                        // quotes: row 190 printed `compiled=42 tried=22 refused=1`
                        // and row 191 `tried=20 refused=0 compiled=40`, both exactly
                        // twice the attempts. Twenty attempts cannot produce forty
                        // compiles, and that inflated pair is what the "42 methods
                        // compiled against C1's 6,675" headline was read from.
                        COMPILED_TOTAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        pr
                    }
                    None => {
                        if let Some(k) = stat_key.as_deref() {
                            self.jit_stats
                                .entry(k.to_string())
                                .or_default()
                                .compile_failed += 1;
                        }
                        crate::compile_fail_census::note(false, &code.bytecode);
                        self.jit_cache.insert(key, jit::JitStatus::Failed);
                        return None;
                    }
                }
            }
            S::Compiled(ptr, ret, sr, mid) => (ptr, ret, sr, mid),
        };
        crate::vm::ExecKernel::new(self).enter_jit(ptr, ret, self_reg, mid, code, class, name, descriptor, is_static, locals, stat_key.as_deref())
    }

    /// The one place `jit_cache` is probed on the entry path. Copies the verdict out — the raw
    /// code pointer included — so no borrow of the map is held across the compile/insert calls
    /// that follow, which is why this exists as a value rather than a reference.
    #[inline]
    fn jit_cache_state(&self, key: usize) -> CacheState {
        match self.jit_cache.get(&key) {
            Some(jit::JitStatus::Compiled(f)) => {
                CacheState::Compiled(f.raw(), f.ret, f.self_reg, f.mid)
            }
            Some(jit::JitStatus::Compiling) => CacheState::Compiling,
            Some(jit::JitStatus::Failed) => CacheState::Failed,
            Some(jit::JitStatus::Cold(n)) => CacheState::Cold(*n),
            None => CacheState::Cold(0),
        }
    }

    /// ⭐**RESOLVE THE INLINE ARMS' FIELD IDS BEFORE COMPILING, NOT FROM THEIR OWN CALLS — AND
    /// ON THE COMPILE DECISION, NOT ON EVERY ENTRY.**
    ///
    /// This is the block that used to sit in `jit_run`'s prologue. Its contract is purely a
    /// COMPILE-ORDER one: each of these ids must be non-zero before the emitter reads it, and the
    /// emitter only ever runs from the compile path (`jit/emit/collections.rs`,
    /// `jit/emit/iterator_inline.rs`). Standing in the per-ENTRY prologue it cost, on every one of
    /// ~21.7 M passages a boot and forever after the ids had settled:
    ///
    ///   * ONE acquire load — `crate::jit::dalist_get_inline_enabled()`, a default-on `OnceLock`;
    ///   * FIVE relaxed loads — `DALIST_A_FID`, `DALIST_SIZE_FID`, `CUBE_PARTS_FID`,
    ///     `ENTRY_KEY_FID`, `ENTRY_VAL_FID`, each a separate static on its own cache line;
    ///   * three branches that are taken exactly once each in the life of the process.
    ///
    /// Called now from the one point both compile paths pass through — immediately after the
    /// warm-up gate proves this method is about to be compiled (`JIT_GATE[7]`, ~18 k passages a
    /// boot) and above `jit_plans`, the background submit and the synchronous emitter alike. That
    /// is strictly no later, relative to any compile, than the old position: the old position was
    /// on the very entry that leads to that compile.
    ///
    /// ⚠What DOES shift is the wall-clock instant at which `resolve_field_id` interns these three
    /// names, hence the numeric ids they receive. Field ids are opaque table indices — the layout
    /// is keyed by `(class, id)` and the hint tables are sized by count — so no observable
    /// behaviour depends on their values, only on their being published before emission.
    /// `RAVA_NO_VM_GATEWORD=1` puts the call back in the prologue if that ever needs bisecting.
    #[cfg(feature = "next-gen-gc")]
    fn prewarm_inline_field_ids(&mut self) {
        // Measured: the arm's emission gate is reached exactly **10 times** in a datapack run and
        // is COLD on all ten (`dalist_gate_cold=10 / ready=0`). Those ten sites compile before any
        // `DoubleArrayList.getDouble` has executed — with tier-0 a method compiles on its FIRST
        // ENTRY — so the ids the natives publish from that very call arrive too late, and a method
        // is compiled once. The bootstrap "0 means not emitted, so it bootstraps itself" cannot
        // converge for an id published by the call the arm exists to serve.
        //
        // Resolving here fixes the ORDER without touching the hot path: once, guarded on the ids
        // still being 0, and only if the class is ALREADY LOADED (`cached`) so this can never
        // trigger a load or perturb resolution the way the driver's compile-time `intern` of
        // `java/util/ArrayList` did — that one measured 13x and is what made these ids
        // publication-only in the first place. `DoubleArrayList` is fastutil with real bytecode
        // and is not on any other lookup path.
        //
        // The CLASS WORD is deliberately not required here: it can only come from a live receiver
        // (interning the name separately yields a different `Arc`, measured as 16,858,961 misses
        // out of 16,858,961), and the emitted code already loads it from the atomic at runtime
        // and refuses on zero.
        #[cfg(feature = "next-gen-gc")]
        if crate::jit::dalist_get_inline_enabled() {
            use std::sync::atomic::Ordering::Relaxed;
            if crate::jit::DALIST_A_FID.load(Relaxed) == 0
                || crate::jit::DALIST_SIZE_FID.load(Relaxed) == 0
            {
                const DAL: &str = "it/unimi/dsi/fastutil/doubles/DoubleArrayList";
                if self.loader.cached(DAL).is_some() {
                    let aid = self.resolve_field_id(DAL, "a");
                    let sid = self.resolve_field_id(DAL, "size");
                    if aid != 0 && crate::jit::DALIST_A_FID.load(Relaxed) == 0 {
                        crate::jit::DALIST_A_FID.store(aid as i64 + 1, Relaxed);
                    }
                    if sid != 0 && crate::jit::DALIST_SIZE_FID.load(Relaxed) == 0 {
                        crate::jit::DALIST_SIZE_FID.store(sid as i64 + 1, Relaxed);
                    }
                }
            }
        }
        // Same for `CubePointRange.parts` — the other half of the `getDouble` population, and the
        // same compile-order trap: its sites compile before the first range exists, so the id it
        // waits for arrives too late. Resolved once, guarded on the id being 0 and the class
        // already loaded, so it can never trigger a load.
        #[cfg(feature = "next-gen-gc")]
        {
            use std::sync::atomic::Ordering::Relaxed;
            // COLD ARM ONLY (`RAVA_NO_CUBECALL=1`). With the round on, no id is resolved by
            // NAME here: the arms' arena edge splices the profiled receiver class's own accessor
            // bytecode (`iterator_inline::emit_value_computer_arm`), their emission gate reads
            // the structural admission count (`fu_fastpath::value_computer_admitted_any`), and
            // the handle cube shape that consumed this id is not emitted. `CUBE_PARTS_FID` and
            // `CUBE_CLASS_WORD` then stay zero for the life of the process.
            if !crate::vm::fu_fastpath::cubecall_enabled() && crate::jit::CUBE_PARTS_FID.load(Relaxed) == 0 {
                const CPR: &str = "net/minecraft/world/phys/shapes/CubePointRange";
                if self.loader.cached(CPR).is_some() {
                    let pid = self.resolve_field_id(CPR, "parts");
                    if pid != 0 {
                        crate::jit::CUBE_PARTS_FID.store(pid as i64 + 1, Relaxed);
                    }
                }
            }
        }
        // `$key`/`$val` for the `Map$Entry` inline arm — synthetic fields, interned by whoever
        // built the entry, so `field_id_existing` answers without loading anything. Resolved
        // BEFORE compiling for the same reason the fastutil ids are: a method is compiled once,
        // and an id published by the calls the arm serves arrives too late.
        #[cfg(feature = "next-gen-gc")]
        {
            use std::sync::atomic::Ordering::Relaxed;
            if crate::jit::ENTRY_KEY_FID.load(Relaxed) == 0 {
                if let Some(id) = self.heap.field_id_existing("$key") {
                    crate::jit::ENTRY_KEY_FID.store(id as i64 + 1, Relaxed);
                }
            }
            if crate::jit::ENTRY_VAL_FID.load(Relaxed) == 0 {
                if let Some(id) = self.heap.field_id_existing("$val") {
                    crate::jit::ENTRY_VAL_FID.store(id as i64 + 1, Relaxed);
                }
            }
        }
    }

    /// **THE PER-CLASS COMPILE REFUSAL, ASKED ONCE PER METHOD INSTEAD OF ONCE PER ENTRY — AND
    /// NARROWED TO THE ONE ARM OF IT THAT IS A NAME SUBSTRING.** Kill switch
    /// `RAVA_NO_WIDE_TIER=1` restores both halves exactly.
    ///
    /// ⭐**HALF ONE: THE SAME ANSWER, FIFTEEN THOUSAND TIMES INSTEAD OF TWENTY-ONE MILLION.**
    /// A product boot makes ~21.7 M passages through `jit_run` (14.1 M interpreted entries +
    /// 7.6 M compiled ones) and every one of them ran `jit_native_served_deep`, whose memo is a
    /// `FastMap<Box<str>, bool>` — a hash of a 40-60 byte class name, per ENTRY, for an answer
    /// that is a property of the CLASS and never changes. 88.5% of those passages land in the
    /// **24 methods** this refusal turns away, and 7.6 M more land in methods that had already
    /// passed it and could not possibly fail it now.
    ///
    /// So the verdict is recorded where every other verdict about this method already lives: the
    /// `jit_cache`, keyed by the `Code` pointer. A refused method becomes `Failed` — which is
    /// exactly what it is, "never retry" — and every later entry returns from the `S::Failed` arm
    /// below without touching a string. The test is re-asked only for `None`/`Cold` entries,
    /// which are by construction the methods not yet decided; that is also what keeps
    /// `force_compile` gated, since it primes a `Cold` entry before calling `jit_run`.
    ///
    /// No new table and no new staleness: the memo this bypasses was already global and permanent
    /// for the process, and reusing `jit_cache` inherits the one lifetime assumption this whole
    /// file already makes (a `Code` address identifies a method for the life of the process).
    /// Removing it puts a class-name hash back on all 21.7 M entries.
    ///
    /// ⭐**HALF TWO: `is_collection` IS A SUBSTRING TEST, AND THE TYPE SYSTEM KNOWS BETTER** —
    /// see `wide_tier_admits`.
    ///
    /// Returns `true` = refuse (the caller returns `None` and the method interprets).
    ///
    /// `probe_state` = "make your own `jit_cache` probe". On the fast path the caller already
    /// holds this method's verdict — it needs it two dozen lines later — and only calls this at
    /// all for a `None`/`Cold` entry, so the probe below was simply the SECOND read of one map
    /// entry on the entry path. It survives for the kill-switch arms, which have no probe to
    /// hand over. Nothing about WHAT is decided changes: the same states answer the same way.
    #[cfg(feature = "next-gen-gc")]
    fn native_served_compile_refusal(
        &mut self,
        gw: u8,
        key: usize,
        class: &Arc<LoadedClass>,
        probe_state: bool,
    ) -> bool {
        if probe_state
            && matches!(
                self.jit_cache.get(&key),
                Some(
                    jit::JitStatus::Compiled(_)
                        | jit::JitStatus::Compiling
                        | jit::JitStatus::Failed
                )
            )
        {
            // Already decided. `Compiled`/`Compiling` passed this test while they were `Cold`;
            // `Failed` carries a refusal already. In all three the state machine below acts on
            // the CACHE and not on this call, so answering "do not refuse" here is neutral.
            return false;
        }
        let cls = class.file.this_class.as_str();
        if !(self.jit_native_served_deep(cls)
            || crate::owner_bits::facts(cls) & crate::owner_bits::FACT_ENGINE_SYNTHETIC != 0)
        {
            return false;
        }
        // A GIL-free worker must not decide this. `wide_tier_admits` reads the TYPE CLOSURE, and a
        // worker cannot load: `ClassLoader::type_closure` then returns a FRAGMENT (it declines to
        // memoise it, but it still returns it), and a fragment is missing exactly the supertypes
        // the admission looks for — it would fail OPEN. A worker never compiles anyway, so it
        // takes the old answer and records nothing; the owning thread asks again with a hierarchy
        // it can actually walk.
        let decidable = !gil::is_worker() && !crate::parallel::gilfree();
        // ⭐**ONE ANSWER WHERE THERE WERE TWO ACQUIRE LOADS.** `wide_tier_enabled()` — a process
        // constant in a `OnceLock` — was read HERE and again at the record arm below, so a class
        // that reaches this far paid two acquire-ordered loads of one static to ask one question
        // twice. Bound once; the population is every not-yet-decided (`None`/`Cold`) method that
        // lands in a natively-served class, which is by construction the whole refused set on its
        // first passage. Short-circuit order is preserved exactly: the gate is still only read
        // when `decidable`, and under the kill switch it is still read from its own static.
        let wide = decidable
            && if gw & VG_LEGACY != 0 { wide_tier_enabled() } else { gw & VG_WIDE_TIER != 0 };
        if wide && self.wide_tier_admits(cls) {
            return false;
        }
        crate::NATIVE_SERVED_REFUSED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // ⚠`[CBAIL]`'s "refused before any attempt" now counts distinct METHODS, not passages —
        // which is the number that line was always reaching for (it is printed beside a count of
        // distinct methods). The old one was a call count wearing a method count's label.
        if wide {
            self.jit_cache.insert(key, jit::JitStatus::Failed);
        }
        true
    }

    /// ⭐**RAISE COVERAGE ON THE CLASSES THE NATIVES DO NOT SERVE AT ALL — and only those.**
    ///
    /// `jit_native_served_deep` is four arms wide. Three are EXACT NAME LISTS
    /// (`map::is_native_map`, `map::is_entry_type`, `opcodes::is_native_io_class`) plus the guava
    /// `Immutable*` package test — each names a family the native layer really does back with
    /// synthetic `$elems`/`$map` state, and each is left exactly as it is. The fourth,
    /// `stream::is_collection`, is a SUBSTRING search for `List`/`Set`/`Queue`/`Deque`/
    /// `Collection`/`Vector`/`Stack` anywhere in the name, and its own file records two false
    /// positives that had to be patched one at a time BY NAME: `java/util/BitSet`, and then every
    /// JDK `*Exception`/`*Error` once `java/lang/StackOverflowError` answered `toString()` out of
    /// an empty `$elems`. Those two are the ones that produced visible wrong output; the rest of
    /// the false positives are silent and simply never compile.
    ///
    /// The question the substring stands in for — "do this class's instances carry collection
    /// state the natives manage?" — is a TYPE question, and this engine already answers type
    /// questions exactly and memoised, from `ClassLoader::type_closure`. A class that is not a
    /// `Collection`, `Iterable`, `Map`, `Map$Entry`, `Iterator` or stream BY TYPE has no
    /// collection state for the natives to shadow, whatever its name spells.
    ///
    /// ⭐The clearest members of the admitted population are the STATIC FACTORY HOLDERS:
    /// `com/google/common/collect/Lists` (48,218 interpreted entries a boot — fifth in the
    /// interpreted census, and refused today for the four letters `List` in its name), `Sets`,
    /// `Queues`, `java/util/Collections`, fastutil's `IntLists`/`ObjectSets`. They are never
    /// instantiated, and a class with no receiver cannot have a receiver the natives serve —
    /// which is the ONE hole that retired `RAVA_NS_BYTECODE` ("a method can be interpreted for
    /// SOME receivers while the natives serve it for OTHERS"). Here that hole is closed by
    /// construction rather than by argument.
    ///
    /// ⚠**AND THIS IS DELIBERATELY NOT THE PER-METHOD `native_claimed` SWITCH, WHICH MEASURED
    /// +2.54%.** That one admitted a whole served namespace minus the methods the cascade had
    /// answered, so it compiled `DoubleArrayList.equals` and guava's indexed iterators — real
    /// collections, 790,000 interpreted entries that became compiled ones and bought nothing,
    /// because what those cost is the INVOKE and not their opcodes. None of that population is
    /// admitted here: every one of those classes IS a `Collection` by type.
    ///
    /// `wide_tier_admits` relaxes only when the full type-closure proof rules out every native
    /// receiver contract for this class. This is independent of an interpreted native decline;
    /// compiled callers may bypass that cascade, so their direct/inline/cache gates stay strict.
    #[cfg(feature = "next-gen-gc")]
    fn wide_tier_admits(&self, cls: &str) -> bool {
        // Exact provider owners read the owner_bits FACTS word — one memo probe per class
        // instead of repeated table scans.
        //
        // The guava `Immutable*` family carries NO arm of its own here, name or fact:
        // every class a native can answer — the provider-union members (`ImmutableMap`
        // → `Map`, `ImmutableEntry` → `Map$Entry`) and the collection impls
        // (`ImmutableList`/`ImmutableSet` → `Collection`) — is refused by the closure
        // check below, and a closure-clean guava class (the `ImmutableTable`/
        // `ImmutableRangeMap` families, Builders, SerializedForms) implements no JDK
        // type a native holds a receiver contract for. The package+contains literal
        // this site used to carry refused exactly that closure-clean residue, on name
        // alone.
        let facts = crate::owner_bits::facts(cls);
        if facts
            & (crate::owner_bits::FACT_NATIVE_MAP
                | crate::owner_bits::FACT_ENTRY_TYPE
                | crate::owner_bits::FACT_IO_CLASS_EXACT
                | crate::owner_bits::FACT_ENGINE_SYNTHETIC)
            != 0
        {
            return false;
        }
        // Every supertype whose instances the native collection/map/stream layer backs with
        // synthetic state. `Iterable` sits beside `Collection` on purpose: fastutil and guava hand
        // out bare iterators (`AbstractIndexedListIterator`) that are not Collections and are
        // served all the same. Six `&str` in rodata plus one hash-set membership test each, run
        // once per refused CLASS (hundreds a boot, never per entry) — no table, no state, nothing
        // that competes for L1.
        const NATIVE_SUPERTYPES: [&str; 6] = [
            "java/util/Collection",
            "java/lang/Iterable",
            "java/util/Map",
            "java/util/Map$Entry",
            "java/util/Iterator",
            "java/util/stream/BaseStream",
        ];
        let closure = self.loader.type_closure(cls);
        // A closure of one is a class whose own supers would not load: no evidence, so refuse.
        // Every real closure contains at least `java/lang/Object` beside the class itself.
        if closure.len() <= 1 {
            return false;
        }
        // ⭐**EXPERIMENT (`RAVA_WIDE_TIER_ALL=1`): admit a class the INTERPRETER ALREADY RUNS.**
        //
        // The supertype list below refuses any class whose closure names a natively-backed type,
        // on the grounds that such a class's own bytecode might read state the natives never
        // populate. `[INTERP-WHO]` on a warm reload refutes that for the population it costs:
        // `gson/LinkedTreeMap.findByObject` takes **368,429 interpreted entries**,
        // `AbstractIndexedListIterator.hasNext` 74,234, `ObjectOpenHashSet$SetIterator.hasNext`
        // 69,196, `ImmutableMapEntry.getKey/getValue` 45,308+34,748 — every one of them RUNNING,
        // interpreted, to a correct answer (the hash is exact). The natives do not serve those
        // methods at all; the class's own bytecode is what executes. Compiling the same bytecode
        // the interpreter already executes cannot change what it reads.
        //
        // So this flag prices the widest form of that argument in one experiment. If it pays, the
        // shipped rule must be the narrow one — refuse per METHOD (does a native serve this exact
        // owner/name/descriptor?) rather than per CLASS — and this flag is the ceiling for it.
        // Gates, 23 probes and both bench hashes are the correctness bar it has to clear.
        if wide_tier_all() {
            return true;
        }
        !NATIVE_SUPERTYPES.iter().any(|t| closure.contains(*t))
    }
}

/// `RAVA_NO_WIDE_TIER=1` — restore the old tiering coverage exactly: the per-class native-served
/// refusal re-derived on every entry and never narrowed, and the worker's hot-method hand-off
/// (`Vm::note_worker_hot`) off. Read ONCE into a `OnceLock`: this sits on the invoke path and a
/// `getenv` on Windows is an env-block query behind a lock plus a UTF-16 encode of the name.
/// `RAVA_WIDE_TIER_ALL=1` — the experiment above: admit every class the native-supertype list
/// refuses. Default OFF; read once.
fn wide_tier_all() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_WIDE_TIER_ALL").is_some())
}

/// First-entry JIT: compile every method on its first entry instead of waiting for
/// the warm-up counter. **Default-ON under Tier-0** (BootSlice82); `RAVA_NO_FIRST_ENTRY_JIT=1`
/// or `RAVA_NO_EMIT_MORE=1` restores threshold mode, so one binary A/Bs both. Structural
/// eligibility (native-served, denied, cap, planning) still applies — only the call-count
/// gate is bypassed. The wall A/B is DISJOINT 4/4 on three PGO products (head4/head5/head6),
/// launch->Done ~18.8-19.4 vs ~19.8-20.8, mean ~-1.3 s; `Done` guest-phase also drops
/// (~0.23 s vs ~0.30 s) because invoke-dense datapack code runs compiled. No-adapt clean:
/// a global count gate, no name/class/timing input.
pub(crate) fn first_entry_compile_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        crate::flags::jit_tier0()
            && std::env::var_os("RAVA_NO_FIRST_ENTRY_JIT").is_none()
            && !crate::flags::emit_more_disabled()
    })
}

pub(crate) fn wide_tier_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_WIDE_TIER").is_none() && !crate::flags::emit_more_disabled()
    })
}

#[cfg(test)]
mod compile_only_queue_tests {
    use super::compile_only_can_queue;
    use crate::{COMPILE_ASYNC_ONLY_KEY, COMPILE_ONLY};

    #[test]
    fn compile_only_queue_is_limited_to_the_authorized_code_key() {
        let previous_compile_only = COMPILE_ONLY.with(|flag| flag.replace(true));
        let previous_async_key = COMPILE_ASYNC_ONLY_KEY.with(|key| key.replace(42));

        assert!(compile_only_can_queue(42));
        assert!(!compile_only_can_queue(43));

        COMPILE_ASYNC_ONLY_KEY.with(|key| key.set(previous_async_key));
        COMPILE_ONLY.with(|flag| flag.set(previous_compile_only));
    }

    #[test]
    fn ordinary_compile_only_requests_remain_synchronous() {
        let previous_compile_only = COMPILE_ONLY.with(|flag| flag.replace(true));
        let previous_async_key = COMPILE_ASYNC_ONLY_KEY.with(|key| key.replace(0));

        assert!(!compile_only_can_queue(42));

        COMPILE_ASYNC_ONLY_KEY.with(|key| key.set(previous_async_key));
        COMPILE_ONLY.with(|flag| flag.set(previous_compile_only));
    }

    #[test]
    fn non_compile_only_requests_can_queue() {
        let previous_compile_only = COMPILE_ONLY.with(|flag| flag.replace(false));
        let previous_async_key = COMPILE_ASYNC_ONLY_KEY.with(|key| key.replace(0));

        assert!(compile_only_can_queue(42));

        COMPILE_ASYNC_ONLY_KEY.with(|key| key.set(previous_async_key));
        COMPILE_ONLY.with(|flag| flag.set(previous_compile_only));
    }
}
