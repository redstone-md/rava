//! The M3 isolation probes, behind ONE armed check.
//!
//! Five probes used to bracket every interpreted call from the body of `invoke_idx`: `m3prof`,
//! `m3shape`, `m3engine`, `m3heap`, `m3biome`. Each was a separate `enabled()` read and — for four
//! of them — a separate local guard with its own `Drop`. All five are OFF in every shipped
//! configuration, so what production paid was five statics, five drop flags, and the unwind
//! funclets those destructors force MSVC to emit around the hot call path; `_NLG_Return2`, the
//! funclet return thunk, was a visible 1.09% row in the microbench profile.
//!
//! They are now one `u8` of armed bits and one guard. Disarmed, the whole block is a single load,
//! a predictable branch and a zero. Armed, the bodies are unchanged and run in the same order —
//! the arming path is `#[cold]` and out of line, which is where five probes' worth of string
//! comparisons belong.

use crate::*;

/// Bit per probe, in ENTER order. Exit runs in the reverse order, which is what the four separate
/// guards did by falling out of scope bottom-up.
const P_PROF: u8 = 1 << 0;
const P_SHAPE: u8 = 1 << 1;
const P_HEAP: u8 = 1 << 2;
const P_BIOME: u8 = 1 << 3;

/// Is ANY probe armed? One `OnceLock` for the five, read once per call.
///
/// Memoised rather than re-read because every one of the five reads its own env var, and the
/// point of the collapse is that a disarmed build touches one cache line, not five.
#[cfg(feature = "next-gen-gc")]
fn any_armed() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        crate::m3prof::enabled()
            || crate::m3shape::enabled()
            || crate::m3engine::enabled()
            || crate::m3heap::enabled()
            || crate::m3heap::worldgen_probe()
            || crate::m3biome::enabled()
    })
}

#[cfg(not(feature = "next-gen-gc"))]
fn any_armed() -> bool {
    crate::m3prof::enabled()
}

/// Whatever the probes entered, unwound in reverse.
pub(crate) struct ProbeScope(u8);

impl Drop for ProbeScope {
    fn drop(&mut self) {
        if self.0 == 0 {
            return;
        }
        self.exit();
    }
}

impl ProbeScope {
    /// Disarmed: the value every shipped call builds.
    pub(crate) const NONE: ProbeScope = ProbeScope(0);

    #[cold]
    fn exit(&mut self) {
        #[cfg(feature = "next-gen-gc")]
        {
            if self.0 & P_BIOME != 0 {
                crate::m3biome::exit();
            }
            if self.0 & P_HEAP != 0 {
                crate::m3heap::exit();
            }
            if self.0 & P_SHAPE != 0 {
                crate::m3shape::exit_initcache();
            }
        }
        if self.0 & P_PROF != 0 {
            crate::m3prof::exit();
        }
    }
}

/// Enter every armed probe for this call. One branch when none is armed.
#[inline(always)]
pub(crate) fn enter(
    vm: &mut Vm,
    class: &crate::class_loader::LoadedClass,
    name: &str,
    code: &crate::Code,
    args: &[Value],
) -> ProbeScope {
    if !any_armed() {
        return ProbeScope::NONE;
    }
    enter_armed(vm, class, name, code, args)
}

/// The original five bodies, verbatim in their original order, out of line.
#[cold]
fn enter_armed(
    vm: &mut Vm,
    class: &crate::class_loader::LoadedClass,
    name: &str,
    code: &crate::Code,
    args: &[Value],
) -> ProbeScope {
    let _ = (&class, name, code, args);
    let mut bits = 0u8;
    if crate::m3prof::enabled() && crate::m3prof::enter(&class.file.this_class, name, &code.bytecode) {
        bits |= P_PROF;
    }
    // M3 9.2a shape-cache cross-state isolation probe: track initCache(state X) scope so field
    // writes/reads can be attributed to X. Receiver = args[0] (instance method). Arena states only.
    #[cfg(feature = "next-gen-gc")]
    if crate::m3shape::enabled()
        && name == "initCache"
        && class.file.this_class.contains("BlockStateBase")
    {
        if let Some(Value::Obj(p)) = args.first() {
            crate::m3shape::enter_initcache(p.addr(), crate::nextgen::arena_class_id(*p));
            bits |= P_SHAPE;
        }
    }
    // M3 9.2c.1 engine-mutation probe: snapshot the shared engine caches at each initCache enter
    // (diffed vs the previous enter → growth attributed to the prior call; call #1 = pre-warm).
    #[cfg(feature = "next-gen-gc")]
    if crate::m3engine::enabled()
        && name == "initCache"
        && class.file.this_class.contains("BlockStateBase")
    {
        crate::m3engine::observe(crate::m3engine::EngineSnap {
            jit: vm.jit_cache.len(),
            direct: vm.jit_direct_cache.len(),
            #[cfg(all(windows, target_arch = "x86_64"))]
            cnp: vm.cnp_cache.len() + vm.aot_clif_cache.len(),
            #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
            cnp: vm.cnp_cache.len(),
            #[cfg(not(all(any(windows, target_os = "linux"), target_arch = "x86_64")))]
            cnp: 0,
            heap: vm.heap.heap_alloc_count(),
            classes: vm.loader.loaded_count(),
        });
    }
    // M3 9.2c.2a heap-allocation classifier: bracket the initCache subtree so heap allocs (+ lazy
    // compiles) fired inside it are attributed to a measured (#>=2) call.
    #[cfg(feature = "next-gen-gc")]
    {
        let is_shape = crate::m3heap::enabled()
            && name == "initCache"
            && class.file.this_class.contains("BlockStateBase");
        let is_worldgen = crate::m3heap::worldgen_probe()
            && (name == "getNoiseBiome" || name.starts_with("findBiome"));
        if is_shape || is_worldgen {
            crate::m3heap::enter();
            bits |= P_HEAP;
        }
    }
    // M3 9.2d.1 biome RTree isolation probe: bracket a getNoiseBiome/findBiome query so writes
    // inside it can be classified pre-phase (shared tree) vs in-phase (local temp).
    #[cfg(feature = "next-gen-gc")]
    if crate::m3biome::enabled()
        && (name == "getNoiseBiome"
            || name.starts_with("findBiome")
            // 10.2 step-0: also bracket DensityFunction.compute (the ChunkFill density path) to
            // prove it read-only — a density-cache wrapper (Cache2D/CacheOnce/FlatCache) writing
            // a PRE-PHASE node's field surfaces as a shared-gen write → the fork hazard.
            || (name == "compute" && class.file.this_class.contains("DensityFunction")))
    {
        let (base, top, _) = crate::nextgen::young_extent();
        crate::m3biome::enter(crate::heap::current_alloc_gen(), (base + top) as u64);
        bits |= P_BIOME;
    }
    ProbeScope(bits)
}
