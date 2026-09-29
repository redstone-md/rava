//! The optimizing tier: bytecode to an SSA graph (`build.rs`), linear-scan register allocation
//! (`regalloc.rs`) and code generation (`codegen*.rs`). Methods using anything the builder does
//! not support are refused and keep the baseline compiler's code. See `docs/core/opt-tier.md`.

mod build;
mod build_mh;
mod build_ops;
mod canon;
mod codegen;
mod codegen_intr;
mod codegen_obj;
mod codegen_ops;
mod deopt;
mod inline;
mod ir;
mod moves;
mod regalloc;

use super::{code_cache, CompiledMethod};
use crate::method::Method;

fn mode() -> &'static str {
    static MODE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    MODE.get_or_init(|| std::env::var("RAVA_CORE_OPT").unwrap_or_default())
}

/// Whether hot methods go to this tier (`RAVA_CORE_OPT=0` keeps baseline tier-2 code).
pub fn enabled() -> bool {
    mode() != "0"
}

/// `RAVA_CORE_OPT=all`: every compilation tries this tier first (testing).
pub fn everywhere() -> bool {
    mode() == "all"
}

/// Deoptimizations of one compiled version before its method is recompiled, and the
/// recompilations before it keeps the baseline compiler's tier-2 code instead.
const DEOPTS_BEFORE_RECOMPILE: u32 = 8;
const MAX_RECOMPILES: u32 = 4;

/// Counts a deoptimization of `m`'s optimized code. Frequent ones recompile it: constants have
/// been resolved and profiles updated since; a method that keeps deoptimizing gets baseline
/// code (which handles exceptions and every instruction itself).
pub(super) fn note_deopt(m: &'static Method) {
    use std::sync::atomic::Ordering;
    let Some(cm) = m.compiled().filter(|c| c.tier >= 2) else { return };
    if cm.deopts.fetch_add(1, Ordering::Relaxed) + 1 != DEOPTS_BEFORE_RECOMPILE {
        return;
    }
    let again = m.deopt_recompiles.fetch_add(1, Ordering::Relaxed) < MAX_RECOMPILES;
    if let Some(cm) = again.then(|| compile(m)).flatten().or_else(|| super::compile::compile(m, 2)) {
        m.install(cm);
    }
}

/// Compiles `m`, or `None` when some instruction is not supported.
pub fn compile(m: &'static Method) -> Option<CompiledMethod> {
    if m.is_synchronized() || m.is_native() {
        return None;
    }
    let started = std::time::Instant::now();
    let cm = compile_timed(m);
    if let Some(s) = super::stats::get() {
        s.opt_nanos.fetch_add(started.elapsed().as_nanos() as u64, std::sync::atomic::Ordering::Relaxed);
    }
    cm
}

fn compile_timed(m: &'static Method) -> Option<CompiledMethod> {
    let log = std::env::var_os("RAVA_CORE_OPT_LOG").is_some();
    let Some((g, extent)) = build::build(m) else {
        if log {
            eprintln!("[opt] refused {m}");
        }
        return None;
    };
    if std::env::var_os("RAVA_CORE_OPT_IR").is_some_and(|f| m.to_string().contains(f.to_str().unwrap_or(""))) {
        eprintln!("[opt-ir] {m}\n{}", g.dump());
    }
    let al = regalloc::allocate(&g);
    let code = codegen::generate(&g, &al, m, extent);
    let entry = code_cache::install(&code);
    code_cache::perf_map(entry, code.len(), &m.to_string());
    if log {
        eprintln!("[opt] {m}: {} values, {} blocks, {} spill slots -> {} bytes", g.values.len(), al.order.len(), al.spill_slots, code.len());
    }
    if std::env::var_os("RAVA_CORE_JIT_DUMP").is_some_and(|f| m.to_string().contains(f.to_str().unwrap_or(""))) {
        let file = format!("/tmp/rava-opt-{entry:x}.bin");
        let _ = std::fs::write(&file, &code);
        eprintln!("[opt-dump] {m} -> {file}");
    }
    Some(CompiledMethod { entry, unresolved_deferrals: Default::default(), size: code.len(), blocks: Vec::new(), frame_slots: extent as usize, tier: 2, deopts: Default::default() })
}
