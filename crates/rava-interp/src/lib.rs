//! Bytecode interpreter. Now multi-class: a `ClassLoader` resolves classes
//! from a directory and/or jar, an object `Heap` backs instances, and class
//! `<clinit>` runs once on first use. The opcode loop lives in `opcodes.rs`.
//!
//! Coverage is still a growing subset — unknown opcodes and unresolved natives
//! fail loudly so the engine extends against real bytecode, never guesses.

mod counters;
pub(crate) use counters::*;

mod fxhash;
pub(crate) use fxhash::*;

mod scratch;
pub use scratch::*;

mod flags;
pub(crate) use flags::*;

use rava_classfile::{Code, Constant};
use std::sync::Arc;
pub use value::{java_dtoa, java_ftoa, java_hex_double, java_hex_float, java_plain_decimal, Value};
pub use natives::set_user_properties;
pub use heap::{Heap, Lambda, LambdaSite};
pub use class_loader::{ClassLoader, LoadError, LoadedClass};
mod vecgrow;
mod gates;
pub(crate) use gates::*;

mod argcall;
/// Narrow-box identity: `Character`/`Short`/`Byte` as REAL objects (kill switch `RAVA_NO_BOXID`).
mod boxid;
pub mod compile_fail_census;
pub mod interp_who;
pub mod diag;
pub(crate) use argcall::*;

mod vmframe;
pub use vmframe::*;
mod vmstate;
pub use vmstate::*;

mod vm;
mod arena;
mod background_jit;
mod jit_phase_trace;
mod cache;
#[cfg(all(any(windows, target_os = "linux"), target_arch = "x86_64"))]
mod cnp;
// The CNP object linker and relocatable Cranelift AOT path support x86-64 Windows/Linux.
// LLVM AOT remains platform-gated separately because it depends on its toolchain integration.
#[cfg(all(feature = "next-gen-gc", any(windows, target_os = "linux"), target_arch = "x86_64"))]
mod aot_clif;
#[cfg(all(feature = "next-gen-gc", any(windows, target_os = "linux"), target_arch = "x86_64"))]
pub use aot_clif::{prepared, ClassCompiler};
#[cfg(all(feature = "next-gen-gc", windows, target_arch = "x86_64"))]
mod llvm_aot;
mod class_loader;
mod cow_shadow;
pub mod prof;
mod descriptor;
mod detached_task;
mod gilfree_cf;
mod execution_state;
mod fallback_tally;
mod freeze_tally;

// `jit_run` compiles the method without executing it for an explicit dispatch request
// or worker-lane preparation; see `COMPILE_ONLY` and `Vm::force_compile`.
thread_local! {
    static COMPILE_ONLY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    // Compile-only dispatch requests may queue this exact code body without executing it.
    // Zero means ordinary compile-only callers retain their synchronous contract.
    static COMPILE_ASYNC_ONLY_KEY: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };

}

mod task_runtime;
mod taskgraph_probe;
mod fastutil;
#[cfg(feature = "next-gen-gc")]
mod fu_view;
#[cfg(test)]
#[path = "fork_batch_tests/mod.rs"]
mod fork_batch_tests;
mod gson_native;
#[cfg(feature = "wall-profile")]
#[path = "rava_sampler/mod.rs"]
pub mod rava_sampler;
#[cfg(not(feature = "wall-profile"))]
#[path = "rava_sampler_stub.rs"]
pub mod rava_sampler;
mod heap;
mod heap_table_trim;
#[cfg(feature = "next-gen-gc")]
mod hof_fusion;
mod jit;
#[allow(unused_imports)]
pub use jit::emit::ops::fields::site_census as field_site_census;
pub use jit::emit::invoke_site_census;
pub use jit::bce_census;
pub use jit::shape_census as jit_shape_census;
pub use opcodes::arrays::arraycopy_census;
mod map;
mod jit_cb_census;
mod monitor;
mod native_state;
mod native_contract;
mod natives;
mod gc_policy;
pub use natives::bridgestat;
pub use natives::dispatch_gate_dump;
#[cfg(feature = "next-gen-gc")]
mod nextgen;
#[cfg(not(feature = "next-gen-gc"))]
#[path = "nextgen_stub.rs"]
mod nextgen;
#[cfg(feature = "next-gen-gc")]
mod arena_map;
#[cfg(feature = "next-gen-gc")]
pub mod parallel_pool;
mod native_claimed;
mod net;
mod nio;
mod offload;
#[cfg(feature = "wall-profile")]
mod offload_probe;
#[cfg(not(feature = "wall-profile"))]
#[path = "offload_probe_stub.rs"]
mod offload_probe;
mod opcodes;
mod optlite;
mod owner_bits;
mod lane_probe;
mod parallel_compile_policy;
mod structural_loop_eager;
mod key_intern;
pub mod flatbody;
pub mod slot_census;
pub mod flatcall;
pub mod shape_census;
pub use frames::pool_census_report;
pub use opcodes::hot_census::report as hot_census_report;
pub use opcodes::hot_census::cold_report as hot_cold_report;
pub use opcodes::hot_census::class_report as hot_class_report;
pub mod pc_profile;

/// `RAVA_PAIR_CENSUS` — adjacent-opcode-pair frequency, for choosing superinstructions.
pub mod pair_census;

/// Name the faulting instruction when the process takes an access violation.
pub mod crash_handler;

/// `RAVA_OPDUMP` — capture a real executed-opcode stream for dispatcher experiments.
pub mod opdump;
mod pc_stack;
mod pilot_census;
mod reflect;
pub mod pure_memo;
pub mod residue_probe;
mod stringify_memo;
mod stream;
/// The JDK's unmodifiable / fixed-size collection contracts (`$immutable`).
mod contract_stat;
mod immutable;
mod npe;
mod stacktrace;
mod view;
mod comparator_plan;
#[cfg(feature = "next-gen-gc")]
pub mod shadow;
/// Per-call scratch handing the interned lambda-class pointer from `jit_direct_resolve` down to
/// `jit_direct_call`'s guard comparison.
///
/// It lived on `Vm` as `jit_lambda_ptr`, which is a data race the moment two threads dispatch at
/// once: the value is written by one resolve and read by the very next guard check, so a sibling
/// worker writing in between silently redirects a lambda guard. It is scratch, not state — no
/// reader outside that hand-off, nothing to publish — so it belongs to the thread, and being
/// thread-local is also what lets `jit_direct_resolve` take `&self`.
pub(crate) mod jit_lambda_scratch {
    use std::cell::Cell;
    thread_local! {
        static PTR: Cell<usize> = const { Cell::new(0) };
    }
    pub(crate) fn set(v: usize) {
        PTR.with(|c| c.set(v));
    }
    pub(crate) fn get() -> usize {
        PTR.with(Cell::get)
    }
}

mod shared_call_cache;
mod worker_direct;
mod tl_worker;
#[allow(dead_code)] // tracer-bullet + foundation for the explicit-stack interpreter
mod tls_cluster;
mod trampoline;
mod value;
mod callcensus;
mod backedge;
mod dresolve;
/// Memoised presence test for a `RAVA_*` env flag, with a PER-SITE `OnceLock`.
///
/// `std::env::var_os` on Windows is `RtlQueryEnvironmentVariable`: a linear walk of the process
/// environment block, under a lock, allocating an `OsString` for the hit. That is fine once and
/// ruinous per call — and this tree had it on paths that run millions of times a boot. The
/// `div`/`rem` callback alone is measured at **9,508,179 calls** (its own comment says so), and
/// it read an un-memoised flag on every one of them.
///
/// The macro exists because the fix must be per-site: a shared name->bool map would trade a PEB
/// walk for a hash lookup, when the correct cost is zero after the first call.
///
/// A diagnostic that is OFF must cost nothing. This makes that true by construction.
macro_rules! env_flag {
    ($name:literal) => {{
        static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *ON.get_or_init(|| std::env::var_os($name).is_some())
    }};
}
pub(crate) use env_flag;

pub mod alloc_sites;
pub mod mem_meter;

pub mod gil;

pub(crate) mod frames;

pub(crate) mod parallel;

pub(crate) mod sitemono;
pub mod devirt;

pub mod jitsig;

pub mod picprobe;

pub mod refpot;

pub mod directmono;

pub mod directprobe;

pub mod selfreg;

pub mod heappic;

pub mod m3prof;

pub mod m3shape;

pub mod m3engine;

pub mod m3heap;

pub mod allochisto;

pub mod enum_birth;

pub mod icmiss;

pub mod bootdisp;

pub mod regiondiag;

pub mod datapack_cache;

pub mod recordast;

pub mod fork_scan;

pub mod structkey_scan;

pub mod escape_scan;

pub mod codec_flatten;

pub mod kernelscan;

pub mod m3biome;

#[cfg(test)]
mod cow_bloom_tests;

#[cfg(all(test, feature = "next-gen-gc"))]
mod parallel_safepoint_tests;

pub(crate) mod tlcache;
pub use tlcache::resolve_census;
#[cfg(test)]
mod tlcache_tests;

/// GAP-S01: the proved iterator-fork batch. `classes` is every DISTINCT receiver
/// class observed in the batch (each must independently pass the per-class effect
/// and whole-tree-compiled proofs — deduplication never drops a class from proof
/// coverage); `code` is the single bytecode method (`Code` pointer identity) that
/// normal runtime resolution selected for ALL of them.
pub(crate) struct ForkBatchTarget {
    pub(crate) classes: Vec<String>,
    pub(crate) code: usize,
}

#[cfg(all(test, feature = "next-gen-gc"))]
mod arena_root_tests;

/// A serializable activation — a `Frame` with the class reduced to its name +
/// the method coordinates, so the live `execute_stackful` frame stack can be
/// frozen to bytes and rebuilt (the class reloads from the jar). `Value`s carry
/// heap handles, valid alongside the matching `Heap::snapshot`.
#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct FrameSnapshot {
    class_name: String,
    name: String,
    desc: String,
    pc: usize,
    locals: Vec<Value>,
    stack: Vec<Value>,
}

#[derive(Default)]
pub(crate) struct JitStats {
    seen: u64,
    cold: u64,
    compiled_runs: u64,
    compile_ok: u64,
    compile_failed: u64,
    cached_failed: u64,
    verify_skipped: u64,
}

/// One cooperative green thread: its suspended explicit-stack call chain plus the
/// `java/lang/Thread` object backing `currentThread()` while it runs.
pub(crate) struct GreenThread {
    pub(crate) frames: Vec<(Frame, Arc<str>, Arc<str>)>,
    pub(crate) thread_obj: u32,
    pub(crate) done: bool,
}

/// Receiver shape recognised by the fastutil scalar fast path: our native `$elems`
/// iterator, or one of the two array-backed int lists (sets, maps, sublists and
/// other impls fall back to the general dispatch).
#[cfg(feature = "next-gen-gc")]
enum FuKind {
    Iter,
    ImmList,
    ArrList,
    DImmList, // it.unimi.dsi.fastutil.doubles.DoubleImmutableList — backing double[] a
    DArrList, // it.unimi.dsi.fastutil.doubles.DoubleArrayList — backing a + logical size
    List, // java.util.ArrayList — native $elems-backed (boxed elements)
    /// java.util.Map$Entry (and the two AbstractMap forms) — `$key`/`$val` fields.
    /// 5,082,022 dispatch misses a boot, #3 and #4 of the whole never-cacheable census.
    Entry,
    /// `net.minecraft.world.phys.shapes.CubePointRange` — a SYNTHETIC `DoubleList` of `parts+1`
    /// evenly spaced values, `getDouble(i) == (double) i / (double) parts`. One `final int` field
    /// and no backing array at all.
    ///
    /// ⭐**7,867,293 of the 7,874,063 refusals `fu_fastpath` logged in a boot are this one
    /// class** (`RAVA_BACKEDGE_CENSUS`) — 12% of all 64,088,346 compiled-to-host crossings, going
    /// the general path for a division. The next-largest refused class is 2,424.
    CubeRange,
    /// `it.unimi.dsi.fastutil.objects.Reference2ObjectArrayMap` — an ARENA object holding three
    /// packed fields (`key[]`, `value[]`, `size`) and comparing keys by REFERENCE.
    ///
    /// ⭐**The largest refused family the arena refusal census names: `#put` 709,683 a datapack
    /// run, `#get` 112,384.** The site is admitted by `fu_map_op` and then declined here, so each
    /// one pays the attempt AND the general dispatch before a native answers it anyway.
    RefArrayMap,
    /// `com.google.gson.internal.LinkedTreeMap$Node` — gson's tree node, an ARENA object whose
    /// `getKey`/`getValue` are one field read each.
    ///
    /// ⭐323,484 refused crossings a datapack run, second only to the array map. It implements
    /// `java.util.Map$Entry`, so the engine already has an arm for the SHAPE — but that one is
    /// bound to the three JDK entry classes by name and this is a fourth.
    TreeNode,
}

/// Argument-list slice of a descriptor: `(II)V` → `II`.
/// Dispatch-IC resolve verdict (see `Vm::dispatch_resolve_cacheable`):
/// `Compiled(ptr)` fills a positive IC entry, `NeverCacheable` fills the -1
/// negative entry, and `NotYet(code_key)` leaves the slot open. A known code
/// key may be memoized only while its JIT state remains `Compiling`.
#[cfg(feature = "next-gen-gc")]
enum DispatchResolve {
    Compiled(usize),
    NeverCacheable,
    NotYet(Option<usize>),
}

/// Re-export so the CLI's end-of-run dump can print the seam census. It printed only from the
/// `pc_profile` dump, and a plain boot does not reach that path — the same trap already recorded
/// for five censuses that printed only from `lane_probe`. This is the table that RANKS the seam,
/// so it is the last one that should require a profiler build to read.
pub fn seam_census_report() {
    if crate::jit_cb_census::enabled() {
        eprintln!("{}", crate::jit_cb_census::report(40));
    }
}

/// Re-export so the CLI's end-of-run dump can print the super-chain census.
pub fn chain_census_report() {
    crate::natives::entry::chain_census::report();
}

/// `RAVA_CENSUS_MS=<n>` — print the end-of-run censuses `n` ms after start and exit.
///
/// ⚠**Every table this engine owns was unreadable for the run that matters, and each time the
/// reason was the PATH, not the flag.** Five censuses printed only from `lane_probe`, which a boot
/// measured to `Done` and then killed never reaches. Moving them to the `pc_profile` dump helped
/// the profiler builds and no others. Moving them to `run.rs` helped the probes, which return from
/// `main` — and not a Minecraft server, which ends inside `System.exit` on a shutdown thread. A
/// harness that kills the process at `Done` defeats all three.
///
/// A timer depends on none of them: it fires wherever the program happens to be.
pub fn start_census_timer_if_on() {
    let Some(ms) = std::env::var("RAVA_CENSUS_MS").ok().and_then(|v| v.trim().parse::<u64>().ok())
    else {
        return;
    };
    std::thread::Builder::new()
        .name("rava-census-timer".into())
        .spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(ms));
            eprintln!("[CENSUS] timer fired at {ms} ms");
            // The host profile has the SAME exit-path problem as the censuses: it dumps when the
            // process ends, and a boot is killed at `Done`. Firing it here is the only way to
            // read a phase that finishes before the program does.
            crate::pc_profile::dump::dump("timer");
            chain_census_report();
            seam_census_report();
            crate::compile_fail_census::report();
            // PER-NATIVE TIME on a product boot. `bridgestat::dump` printed only from `run.rs`,
            // which a Minecraft server never reaches — the same trap as every other table here.
            // This is the one instrument that turns "the natives' own work" into a ranked list.
            crate::natives::bridgestat::dump();
            // WHY a dispatch miss is permanent. The map's largest line (8.5 M crossings, 2.6 s)
            // rests on a recorded claim — "97% reach natives with no compiled body" — that no
            // measurement this session checked. This is that measurement, on a reachable path.
            {
                let r = crate::icmiss::never_why_report();
                if !r.is_empty() {
                    eprintln!("[ICMISS] {r}");
                    let n = crate::icmiss::never_names_report(12);
                    if !n.is_empty() {
                        eprintln!("[ICMISS] classes: {n}");
                    }
                }
            }
            jit_gate_report();
            lookup_count_report();
            crate::structkey_scan::dump();
            use std::io::Write;
            let _ = std::io::stderr().flush();
            std::process::exit(0);
        })
        .ok();
}

/// WHY A METHOD NEVER REACHES THE COMPILER. `JIT_GATE`/`COLD_LEVEL` have existed for a long time
/// and printed from ONE place: `lane_probe`, which a product boot does not reach. That is the
/// sixth table in this engine found unreadable for exactly the run that matters.
pub fn jit_gate_report() {
    use std::sync::atomic::Ordering::Relaxed;
    let g: Vec<usize> = crate::JIT_GATE.iter().map(|a| a.load(Relaxed)).collect();
    let c: Vec<usize> = crate::COLD_LEVEL.iter().map(|a| a.load(Relaxed)).collect();
    let n = crate::COMPILED_TOTAL.load(Relaxed);
    let t = crate::COMPILE_TRIED.load(Relaxed);
    let r = crate::COMPILE_REFUSED.load(Relaxed);
    if g[3] == 0 && n == 0 {
        return;
    }
    eprintln!(
        "[JIT-GATE] entries={} cold_bumps={} passed_warmup={} | tried={t} compiled={n} refused={r}",
        g[3], g[4], g[7]
    );
    eprintln!(
        "[JIT-GATE] denied: deny_list={} native_served_or_cache={} spline_fallback={}",
        g[0], g[1], g[2]
    );
    let pn: Vec<usize> = crate::PLANS_NONE.iter().map(|a| a.load(Relaxed)).collect();
    eprintln!(
        "[JIT-GATE] no compile: total={} planning-none distinct={} | queue FULL={} | cap_refused={}",
        pn[0], pn[1], pn[3], pn[2]
    );
    eprintln!(
        "[JIT-GATE] cold levels >=64:{} >=128:{} >=256:{} >=512:{} | worker_visits={} worker_hot_dropped={}",
        c[0], c[1], c[2], c[3], g[5], g[6]
    );
}

/// STRING-KEYED RESOLUTION VOLUME (`RAVA_LOOKUP_COUNT`). `memchr` is 3.0% of a product boot's
/// host profile and `ClassLoader::super_chain` read 4.0% in a probe — both are the same shape,
/// a cache whose KEY is a class name, hashed on every hit. The counters existed; like the six
/// tables before them they printed only from a path a boot does not reach.
pub fn lookup_count_report() {
    if std::env::var_os("RAVA_LOOKUP_COUNT").is_none() {
        return;
    }
    use std::sync::atomic::Ordering::Relaxed;
    eprintln!(
        "[LOOKUP-COUNT] super_chain={} get_or_load={}",
        crate::class_loader::SUPER_CHAIN_CALLS.load(Relaxed),
        crate::class_loader::GET_OR_LOAD_CALLS.load(Relaxed),
    );
}
