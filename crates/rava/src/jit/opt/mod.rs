//! The optimizing tier: bytecode to an SSA graph (`build.rs`), linear-scan register allocation
//! (`regalloc.rs`) and code generation (`codegen*.rs`). Methods using anything the builder does
//! not support are refused and keep the baseline compiler's code. See `docs/core/opt-tier.md`.

mod algebra;
mod barrier;
mod booleans;
mod build;
mod build_cache;
mod build_arr;
mod build_mh;
mod build_obj;
mod build_ops;
mod canon;
mod cha;
mod codegen;
mod codegen_copy;
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
mod conds;
mod consts;
mod dce;
mod guards;
mod licm;
mod rce;
mod liveness;
mod locals;
mod moves;
mod nullcheck;
pub(crate) mod osr;
mod operands;
mod phis;
mod regalloc;
mod reload;
mod scratch;
mod select;
mod split;
mod thread;
mod typecheck;
mod unroll;

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
    let cm = std::panic::catch_unwind(|| compile_timed(m, None)).unwrap_or_else(|_| {
        eprintln!("[opt] compilation of {m} failed; it stays at the lower tier");
        None
    });
    if let Some(s) = super::stats::get() {
        s.opt_nanos.fetch_add(started.elapsed().as_nanos() as u64, std::sync::atomic::Ordering::Relaxed);
    }
    cm
}

/// Compiles `m` entered at the loop header `bci` from a running frame (`osr.rs`); its entry
/// is the bridge baseline code jumps to.
fn compile_osr(m: &'static Method, bci: u32) -> Option<CompiledMethod> {
    std::panic::catch_unwind(|| compile_timed(m, Some(bci))).unwrap_or_else(|_| {
        eprintln!("[opt] compilation of {m} at bci {bci} failed; the loop stays at the lower tier");
        None
    })
}

/// With `RAVA_OPT_VERIFY`, checks the graph's edges after a pass (`Graph::verify`); a broken
/// graph fails the compilation naming the pass.
fn verify(g: &ir::Graph, m: &'static Method, pass: &str) {
    if env_once!("RAVA_OPT_VERIFY").is_some() {
        if let Err(e) = g.verify() {
            panic!("{m}: graph broken after {pass}: {e}");
        }
    }
}

fn compile_timed(m: &'static Method, osr: Option<u32>) -> Option<CompiledMethod> {
    let log = env_once!("RAVA_OPT_LOG").is_some();
    let monitor = match (m.is_synchronized(), m.is_static()) {
        (false, _) => None,
        _ if osr.is_some() => return None,
        (true, false) => Some(codegen::Monitor::Receiver),
        (true, true) => Some(codegen::Monitor::Mirror(Some(m.holder.mirror()).filter(|o| !o.is_null())? as usize)),
    };
    let built = match osr {
        None => build::build(m),
        Some(bci) => osr::local_types(m, bci as usize).and_then(|types| build::build_osr(m, bci as usize, &types)),
    };
    let Some((mut g, extent)) = built else {
        if log {
            eprintln!("[opt] refused {m}{}", osr.map_or(String::new(), |b| format!(" at bci {b}")));
        }
        return None;
    };
    verify(&g, m, "build");
    phis::simplify(&mut g);
    verify(&g, m, "phis::simplify");
    booleans::fold_masks(&mut g);
    verify(&g, m, "booleans::fold_masks");
    nullcheck::eliminate(&mut g);
    verify(&g, m, "nullcheck::eliminate");
    typecheck::eliminate(&mut g);
    verify(&g, m, "typecheck::eliminate");
    conds::eliminate(&mut g);
    verify(&g, m, "conds::eliminate");
    fields::forward(&mut g);
    verify(&g, m, "fields::forward");
    escape::eliminate(&mut g);
    verify(&g, m, "escape::eliminate");
    select::convert(&mut g);
    verify(&g, m, "select::convert");
    algebra::simplify(&mut g);
    verify(&g, m, "algebra::simplify");
    licm::hoist(&mut g);
    verify(&g, m, "licm::hoist");
    rce::eliminate(&mut g);
    verify(&g, m, "rce::eliminate");
    guards::hoist(&mut g);
    verify(&g, m, "guards::hoist");
    g.merge_chains();
    verify(&g, m, "g.merge_chains");
    thread::thread(&mut g);
    verify(&g, m, "thread::thread");
    g.merge_chains();
    verify(&g, m, "g.merge_chains");
    unroll::unroll(&mut g);
    verify(&g, m, "unroll::unroll");
    algebra::simplify_late(&mut g);
    verify(&g, m, "algebra::simplify_late");
    consts::hoist(&mut g);
    verify(&g, m, "consts::hoist");
    g.prune_unreachable();
    verify(&g, m, "g.prune_unreachable");
    dce::eliminate(&mut g);
    verify(&g, m, "dce::eliminate");
    if env_once!("RAVA_OPT_SPLIT").map_or(true, |v| v != "0") {
        split::split(&mut g);
        verify(&g, m, "split::split");
    }
    g.merge_chains();
    verify(&g, m, "g.merge_chains");
    let dump = env_once!("RAVA_OPT_IR").is_some_and(|f| m.to_string().contains(f.to_str().unwrap_or("")));
    if dump {
        eprintln!("[opt-ir] {m}\n{}", g.dump());
    }
    conds::mark_nonnull_inputs(&mut g);
    verify(&g, m, "conds::mark_nonnull_inputs");
    let mut al = regalloc::allocate(&g);
    if env_once!("RAVA_OPT_RELOAD").map_or(true, |v| v != "0") {
        let mut copies = reload::split(&mut g, &al.loc);
        if !copies.is_empty() {
            al = regalloc::allocate(&g);
            // Each allocation may leave other copies without a register: take those back too.
            for _ in 0..3 {
                if !reload::undo_spilled(&mut g, &copies, &al.loc) {
                    break;
                }
                copies.retain(|&c| matches!(al.loc[c as usize], regalloc::Loc::Reg(_) | regalloc::Loc::Xmm(_)));
                al = regalloc::allocate(&g);
            }
        }
    }
    if dump {
        let locs: Vec<String> = al.loc.iter().enumerate().filter(|(_, l)| !matches!(l, regalloc::Loc::None | regalloc::Loc::Const(_))).map(|(v, l)| match al.home[v] {
            Some(h) => format!("v{v}={l:?}/home {h:?}"),
            None => format!("v{v}={l:?}"),
        }).collect();
        eprintln!("[opt-alloc] {}", locs.join(" "));
    }
    let (code, main, faults) = codegen::generate(&g, &al, m, extent, monitor, osr);
    let entry = code_cache::install(&code);
    if !faults.is_empty() {
        super::fault::register(faults.into_iter().map(|(at, to)| (entry + at, entry + to)));
    }
    let tag = osr.map_or(String::new(), |b| format!(" at bci {b}"));
    code_cache::perf_map(entry, code.len(), &format!("{m} [opt{}]", if osr.is_some() { " osr" } else { "" }));
    if log {
        eprintln!("[opt] {m}{tag}: {} values, {} blocks, {} spill slots -> {} bytes ({main} main)", g.values.len(), al.order.len(), al.spill_slots, code.len());
    }
    if env_once!("RAVA_JIT_DUMP").is_some_and(|f| m.to_string().contains(f.to_str().unwrap_or(""))) {
        let file = format!("/tmp/rava-opt-{entry:x}.bin");
        // SAFETY: the code just installed (calls resolved).
        let _ = std::fs::write(&file, unsafe { std::slice::from_raw_parts(entry as *const u8, code.len()) });
        eprintln!("[opt-dump] {m} -> {file}");
    }
    Some(CompiledMethod { entry, unresolved_deferrals: Default::default(), size: main, blocks: Vec::new(), frame_slots: extent as usize, tier: 2, optimized: true, deopts: Default::default() })
}
