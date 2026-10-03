//! The optimizing tier: bytecode to an SSA graph (`build.rs`), linear-scan register allocation
//! (`regalloc.rs`) and code generation (`codegen*.rs`). Methods using anything the builder does
//! not support are refused and keep the baseline compiler's code. See `docs/core/opt-tier.md`.

mod barrier;
mod build;
mod build_cache;
mod build_arr;
mod build_mh;
mod build_obj;
mod build_ops;
mod canon;
mod codegen;
mod codegen_intr;
mod codegen_obj;
mod codegen_ops;
mod polls;
mod deopt;
mod dom;
mod escape;
mod fields;
mod freq;
mod inline;
mod ir;
mod licm;
mod rce;
mod liveness;
mod locals;
mod moves;
mod nullcheck;
mod operands;
mod phis;
mod regalloc;
mod scratch;
mod select;
mod split;

use super::{code_cache, CompiledMethod};
use crate::method::Method;

fn mode() -> &'static str {
    static MODE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    MODE.get_or_init(|| std::env::var("RAVA_OPT").unwrap_or_default())
}

/// Whether hot methods go to this tier (`RAVA_OPT=0` keeps baseline tier-2 code).
pub fn enabled() -> bool {
    mode() != "0"
}

/// `RAVA_OPT=all`: every compilation tries this tier first (testing).
pub fn everywhere() -> bool {
    mode() == "all"
}

/// Deoptimizations of one compiled version before its method is recompiled, and the
/// recompilations before it keeps the baseline compiler's tier-2 code instead.
const DEOPTS_BEFORE_RECOMPILE: u32 = 8;
const MAX_RECOMPILES: u32 = 8;
/// Recompilations after which a method is compiled without speculating on its profile. Each
/// bytecode that trapped already stops speculating by itself (`profile::trapped`), as C2's
/// per-bytecode trap limits do; giving up on the whole method after two recompilations left
/// paths that were cold at the first compilations with unguarded calls and no inlining.
pub(super) const SPECULATIVE_RECOMPILES: u32 = MAX_RECOMPILES;

/// Counts a deoptimization of `m`'s optimized code. Frequent ones recompile it: constants have
/// been resolved and profiles updated since; a method that keeps deoptimizing gets baseline
/// code (which handles exceptions and every instruction itself).
pub(super) fn note_deopt(m: &'static Method) {
    use std::sync::atomic::Ordering;
    let Some(cm) = m.compiled().filter(|c| c.tier >= 2) else { return };
    if cm.deopts.fetch_add(1, Ordering::Relaxed) + 1 == DEOPTS_BEFORE_RECOMPILE {
        m.retire(cm);
        super::queue::submit(super::queue::Job::Recompile(m));
    }
}

/// Recompiles `m` after frequent deoptimizations.
pub(in crate::jit) fn recompile(m: &'static Method) {
    use std::sync::atomic::Ordering;
    let again = m.deopt_recompiles.fetch_add(1, Ordering::Relaxed) < MAX_RECOMPILES;
    if let Some(cm) = again.then(|| compile(m)).flatten().or_else(|| super::compile::compile(m, 2)) {
        m.install(cm);
    }
}

/// Recompiles `m`'s optimized code with the profiles its cold-at-compile-time call sites
/// recorded since; keeps the current code if that fails.
pub(in crate::jit) fn reprofile(m: &'static Method) {
    if m.compiled().is_some_and(|c| c.tier >= 2) {
        if let Some(cm) = compile(m) {
            m.install(cm);
        }
    }
}

/// Compiles `m`, or `None` when some instruction is not supported.
pub fn compile(m: &'static Method) -> Option<CompiledMethod> {
    if m.is_native() {
        return None;
    }
    let started = std::time::Instant::now();
    // A failure in the compiler abandons this compilation, not the compiler thread; the method
    // keeps running at the lower tier (HotSpot's compilation bailout).
    let cm = std::panic::catch_unwind(|| compile_timed(m)).unwrap_or_else(|_| {
        eprintln!("[opt] compilation of {m} failed; it stays at the lower tier");
        None
    });
    if let Some(s) = super::stats::get() {
        s.opt_nanos.fetch_add(started.elapsed().as_nanos() as u64, std::sync::atomic::Ordering::Relaxed);
    }
    cm
}

fn compile_timed(m: &'static Method) -> Option<CompiledMethod> {
    let log = env_once!("RAVA_OPT_LOG").is_some();
    let monitor = match (m.is_synchronized(), m.is_static()) {
        (false, _) => None,
        (true, false) => Some(codegen::Monitor::Receiver),
        (true, true) => Some(codegen::Monitor::Mirror(Some(m.holder.mirror()).filter(|o| !o.is_null())? as usize)),
    };
    let Some((mut g, extent)) = build::build(m) else {
        if log {
            eprintln!("[opt] refused {m}");
        }
        return None;
    };
    phis::simplify(&mut g);
    nullcheck::eliminate(&mut g);
    fields::forward(&mut g);
    escape::eliminate(&mut g);
    select::convert(&mut g);
    licm::hoist(&mut g);
    rce::eliminate(&mut g);
    if env_once!("RAVA_OPT_SPLIT").map_or(true, |v| v != "0") {
        split::split(&mut g);
    }
    let dump = env_once!("RAVA_OPT_IR").is_some_and(|f| m.to_string().contains(f.to_str().unwrap_or("")));
    if dump {
        eprintln!("[opt-ir] {m}\n{}", g.dump());
    }
    let al = regalloc::allocate(&g);
    if dump {
        let locs: Vec<String> = al.loc.iter().enumerate().filter(|(_, l)| !matches!(l, regalloc::Loc::None | regalloc::Loc::Const(_))).map(|(v, l)| format!("v{v}={l:?}")).collect();
        eprintln!("[opt-alloc] {}", locs.join(" "));
    }
    let (code, main, faults) = codegen::generate(&g, &al, m, extent, monitor);
    let entry = code_cache::install(&code);
    if !faults.is_empty() {
        super::fault::register(faults.into_iter().map(|(at, to)| (entry + at, entry + to)));
    }
    code_cache::perf_map(entry, code.len(), &format!("{m} [opt]"));
    if log {
        eprintln!("[opt] {m}: {} values, {} blocks, {} spill slots -> {} bytes ({main} main)", g.values.len(), al.order.len(), al.spill_slots, code.len());
    }
    if env_once!("RAVA_JIT_DUMP").is_some_and(|f| m.to_string().contains(f.to_str().unwrap_or(""))) {
        let file = format!("/tmp/rava-opt-{entry:x}.bin");
        let _ = std::fs::write(&file, &code);
        eprintln!("[opt-dump] {m} -> {file}");
    }
    Some(CompiledMethod { entry, unresolved_deferrals: Default::default(), size: main, blocks: Vec::new(), frame_slots: extent as usize, tier: 2, deopts: Default::default() })
}
