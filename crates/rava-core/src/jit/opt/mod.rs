//! The optimizing tier: bytecode to an SSA graph (`build.rs`), linear-scan register allocation
//! (`regalloc.rs`) and code generation (`codegen*.rs`). Methods using anything the builder does
//! not support are refused and keep the baseline compiler's code. See `docs/core/opt-tier.md`.

mod build;
mod build_ops;
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

/// Whether hot methods go to this tier (`RAVA_CORE_OPT=1`, until it covers enough bytecode).
pub fn enabled() -> bool {
    !matches!(mode(), "" | "0")
}

/// `RAVA_CORE_OPT=all`: every compilation tries this tier first (testing).
pub fn everywhere() -> bool {
    mode() == "all"
}

/// Compiles `m`, or `None` when some instruction is not supported.
pub fn compile(m: &'static Method) -> Option<CompiledMethod> {
    if m.is_synchronized() || m.is_native() {
        return None;
    }
    let log = std::env::var_os("RAVA_CORE_OPT_LOG").is_some();
    let Some((g, extent)) = build::build(m) else {
        if log {
            eprintln!("[opt] refused {m}");
        }
        return None;
    };
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
    Some(CompiledMethod { entry, unresolved_deferrals: Default::default(), size: code.len(), blocks: Vec::new(), frame_slots: extent as usize, tier: 2 })
}
