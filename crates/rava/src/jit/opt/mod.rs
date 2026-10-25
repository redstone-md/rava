//! The optimizing tier: bytecode to an SSA graph (`build.rs`), linear-scan register allocation
//! (`regalloc.rs`) and code generation (`codegen*.rs`). Methods using anything the builder does
//! not support are refused and keep the baseline compiler's code. See `docs/core/opt-tier.md`.

mod algebra;
mod backedges;
mod barrier;
mod booleans;
mod build;
mod build_cache;
mod build_arr;
mod build_mh;
mod build_obj;
mod build_ops;
mod canon;
mod castdispatch;
mod coarsen;
pub(crate) mod cha;
mod codegen;
mod codegen_cmp;
mod codegen_copy;
mod codegen_intr;
mod codegen_obj;
mod codegen_ops;
mod polls;
mod deopt;
mod fill;
mod dom;
mod escape;
mod fields;
mod flags;
mod freq;
mod inline;
mod ivs;
mod ir;
mod conds;
mod consts;
mod dce;
mod deadstore;
mod guards;
mod gvn;
mod initstores;
mod hints;
mod licm;
mod loadnum;
mod loopfwd;
mod rce;
mod reduce;
mod liveness;
mod locals;
mod moves;
mod nullcheck;
pub(crate) mod osr;
mod parallel;
mod operands;
mod phis;
mod maskcheck;
mod phi_share;
mod regalloc;
mod reload;
mod scratch;
mod select;
mod split;
mod strip;
mod thread;
mod fact_thread;
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

/// `RAVA_OPT_EXCLUDE=a,b`: methods whose `class.name` starts with one of the comma-separated
/// prefixes (`net/foo/`, `java/util/HashMap.get`) are neither compiled by this tier nor inlined
/// (HotSpot's `CompileCommand=exclude`), for narrowing a miscompilation down to a method.
pub fn excluded(m: &Method) -> bool {
    static PREFIXES: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    let prefixes = PREFIXES.get_or_init(|| std::env::var("RAVA_OPT_EXCLUDE").map(|v| v.split(',').map(str::trim).filter(|p| !p.is_empty()).map(String::from).collect()).unwrap_or_default());
    !prefixes.is_empty() && {
        let name = format!("{}.{}", m.holder.name, m.name);
        prefixes.iter().any(|p| name.starts_with(p.as_str()))
    }
}

/// `RAVA_OPT=all`: every compilation tries this tier first (testing).
pub fn everywhere() -> bool {
    mode() == "all"
}

/// Deoptimizations of one compiled version before its method is recompiled (for a failed
/// speculation, and to throw), and the recompilations before it keeps the baseline compiler's
/// tier-2 code instead. A failed speculation recompiles soon, as C2's traps invalidate their
/// code: the trapping bytecode stops speculating (`profile::trapped`), and until then every
/// call taking that path leaves for the interpreter for the rest of its invocation.
const TRAPS_BEFORE_RECOMPILE: u32 = 2;
const DEOPTS_BEFORE_RECOMPILE: u32 = 8;
const MAX_RECOMPILES: u32 = 16;
/// Recompilations after which a method is compiled without speculating on its profile. Each
/// bytecode that trapped already stops speculating by itself (`profile::trapped`), as C2's
/// per-bytecode trap limits do; giving up on the whole method after two recompilations left
/// paths that were cold at the first compilations with unguarded calls and no inlining. A
/// method whose speculations keep failing at new places (a renderer meeting new entity
/// classes) goes on recompiling, as C2's (hundreds of recompilations) do, then stops
/// speculating rather than falling back to baseline code.
pub(super) const SPECULATIVE_RECOMPILES: u32 = 12;

/// Counts a deoptimization of `m`'s optimized code. Frequent ones recompile it: constants have
/// been resolved and profiles updated since; a method that keeps deoptimizing gets baseline
/// code (which handles exceptions and every instruction itself).
pub(super) fn note_deopt(m: &'static Method, trap: bool) {
    use std::sync::atomic::Ordering;
    let Some(cm) = m.compiled().filter(|c| c.tier >= 2) else { return };
    // A trap weighs as several throws: the `TRAPS_BEFORE_RECOMPILE`th recompiles.
    let weight = if trap { DEOPTS_BEFORE_RECOMPILE / TRAPS_BEFORE_RECOMPILE } else { 1 };
    let before = cm.deopts.fetch_add(weight, Ordering::Relaxed);
    if before < DEOPTS_BEFORE_RECOMPILE && before + weight >= DEOPTS_BEFORE_RECOMPILE {
        m.retire(cm);
        super::queue::submit(super::queue::Job::Recompile(m));
    }
}

/// Recompiles `m` after frequent deoptimizations.
pub(in crate::jit) fn recompile(m: &'static Method) {
    use std::sync::atomic::Ordering;
    let again = m.jit().deopt_recompiles.fetch_add(1, Ordering::Relaxed) < MAX_RECOMPILES;
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
    let cm = crate::crash::expected(|| std::panic::catch_unwind(|| compile_timed(m, None))).unwrap_or_else(|_| {
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
    crate::crash::expected(|| std::panic::catch_unwind(|| compile_timed(m, Some(bci)))).unwrap_or_else(|_| {
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

/// Optimizing compilations started, for diagnostics (`RAVA_SAMPLE`'s CPU series).
pub static COMPILES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn compile_timed(m: &'static Method, osr: Option<u32>) -> Option<CompiledMethod> {
    // Both entries (whole methods and loops entered from a running frame) pass here.
    if excluded(m) {
        return None;
    }
    COMPILES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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
    // Only where there are allocations to free from merges.
    if env_once!("RAVA_NO_FACT_THREAD").is_none() && g.values.iter().any(|v| matches!(v.inst, ir::Inst::New(..))) {
        // Before scalar replacement, so the objects these merges no longer merge can be. Each
        // round can open the next: a merge split leaves the merge of the inlined test's boolean
        // (`isPresent`), whose threading lets the object's next merge split. Rounds stop once
        // one changes no edge.
        let edges = |g: &ir::Graph| g.blocks.iter().enumerate().map(|(b, blk)| blk.term.successors().iter().map(|&s| (b + 1) * (s as usize + 7)).sum::<usize>()).sum::<usize>();
        g.merge_chains();
        let mut last = usize::MAX;
        for _ in 0..4 {
            if edges(&g) == last {
                break;
            }
            last = edges(&g);
            fact_thread::thread(&mut g);
            phis::fold_trivial(&mut g);
            thread::thread(&mut g);
            g.merge_chains();
        }
        verify(&g, m, "fact_thread::thread");
    }
    fields::forward(&mut g);
    verify(&g, m, "fields::forward");
    escape::eliminate(&mut g);
    verify(&g, m, "escape::eliminate");
    select::convert(&mut g);
    verify(&g, m, "select::convert");
    algebra::simplify(&mut g);
    verify(&g, m, "algebra::simplify");
    // Threading and folded switches left tests of a class right above its casts (a pattern
    // switch's case reached from its label's test).
    typecheck::eliminate(&mut g);
    verify(&g, m, "typecheck::eliminate");
    if env_once!("RAVA_NO_CASTDISPATCH").is_none() {
        castdispatch::sink(&mut g);
        verify(&g, m, "castdispatch::sink");
    }
    loadnum::number(&mut g);
    verify(&g, m, "loadnum::number");
    // Class words and fields now one value: the branches on them a dominating one decided.
    conds::eliminate(&mut g);
    verify(&g, m, "conds::eliminate (after loadnum)");
    flags::merge(&mut g);
    verify(&g, m, "flags::merge");
    backedges::split(&mut g);
    phis::fold_trivial(&mut g);
    verify(&g, m, "backedges::split");
    licm::hoist(&mut g);
    verify(&g, m, "licm::hoist");
    loopfwd::forward(&mut g);
    verify(&g, m, "loopfwd::forward");

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
    if env_once!("RAVA_NO_FILL").is_none() {
        fill::convert(&mut g);
        verify(&g, m, "fill::convert");
    }
    if env_once!("RAVA_NO_REDUCE").is_none() {
        reduce::convert(&mut g);
        verify(&g, m, "reduce::convert");
    }
    unroll::unroll(&mut g);
    verify(&g, m, "unroll::unroll");
    if env_once!("RAVA_NO_UNROLL4").is_none() {
        unroll::unroll_partially(&mut g);
        verify(&g, m, "unroll::unroll_partially");
    }
    // Unrolled copies repeat their uniqueness checks; a repeat breaks a coarsened region.
    flags::merge(&mut g);
    verify(&g, m, "flags::merge");
    coarsen::coarsen(&mut g);
    verify(&g, m, "coarsen::coarsen");
    algebra::simplify_late(&mut g);
    verify(&g, m, "algebra::simplify_late");
    loadnum::number(&mut g);
    verify(&g, m, "loadnum::number (late)");
    deadstore::eliminate(&mut g);
    verify(&g, m, "deadstore::eliminate");    gvn::number(&mut g);
    verify(&g, m, "gvn::number");
    maskcheck::speculate(&mut g);
    verify(&g, m, "maskcheck::speculate");
    // Late value numbering makes loads one that the branches before it still compared.
    conds::fold_self_compares(&mut g);
    verify(&g, m, "conds::fold_self_compares (late)");
    guards::dedupe(&mut g);
    verify(&g, m, "guards::dedupe");
    consts::hoist(&mut g);
    verify(&g, m, "consts::hoist");
    g.prune_unreachable();
    verify(&g, m, "g.prune_unreachable");
    dce::eliminate(&mut g);
    verify(&g, m, "dce::eliminate");
    parallel::report(&g, m);
    if env_once!("RAVA_NO_STRIP").is_none() {
        strip::mine(&mut g);
        verify(&g, m, "strip::mine");
    }
    if env_once!("RAVA_OPT_SPLIT").map_or(true, |v| v != "0") {
        split::split(&mut g);
        verify(&g, m, "split::split");
    }
    g.merge_chains();
    verify(&g, m, "g.merge_chains");
    let dump = env_once!("RAVA_OPT_IR").is_some_and(|f| m.to_string().contains(f.to_str().unwrap_or("")));
    if dump {
        eprintln!("[opt-ir] {m}
{}", g.dump());
    }
    conds::mark_nonnull_inputs(&mut g);
    verify(&g, m, "conds::mark_nonnull_inputs");
    split::constant_copies(&mut g);
    let mut al = regalloc::allocate(&g, m);
    if env_once!("RAVA_OPT_RELOAD").map_or(true, |v| v != "0") {
        let mut copies = reload::split(&mut g, &al.loc);
        if !copies.is_empty() {
            al = regalloc::allocate(&g, m);
            // Each allocation may leave other copies without a register: take those back too.
            for _ in 0..3 {
                if !reload::undo_spilled(&mut g, &copies, &al.loc) {
                    break;
                }
                copies.retain(|&c| matches!(al.loc[c as usize], regalloc::Loc::Reg(_) | regalloc::Loc::Xmm(_)));
                al = regalloc::allocate(&g, m);
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
    let dumped = env_once!("RAVA_JIT_DUMP").is_some_and(|f| m.to_string().contains(f.to_str().unwrap_or("")));
    // `RAVA_JIT_DUMP_RAW`: the code alone, without the instruction trace (whose marks change no
    // code, `Asm::mark`).
    let raw = env_once!("RAVA_JIT_DUMP_RAW").is_some_and(|f| m.to_string().contains(f.to_str().unwrap_or("")));
    // `RAVA_PERF_MAP=inline`: name the code of each inlined method after it, so samples are
    // attributed to the innermost method as HotSpot's profilers do.
    let by_leaf = env_once!("RAVA_PERF_MAP").is_some_and(|v| v == "inline");
    let (code, main, faults, trace, reg_entry, scopes) = codegen::generate(&g, &al, m, extent, monitor, osr, dumped || by_leaf);
    let entry = code_cache::install_in(&code, code_cache::Zone::Optimized);
    if !faults.is_empty() {
        super::fault::register(faults.into_iter().map(|(at, to)| (entry + at, entry + to)));
    }
    let tag = osr.map_or(String::new(), |b| format!(" at bci {b}"));
    if by_leaf {
        perf_map_by_leaf(entry, code.len(), m, &trace);
    } else {
        code_cache::perf_map(entry, code.len(), &format!("{m} [opt{}]", if osr.is_some() { " osr" } else { "" }));
    }    // After the code is named: a recompilation into the same place replaces both.
    if !scopes.is_empty() {
        crate::profile::code_scopes(entry, scopes.iter().map(|(at, o)| (*at, ir::frames_of(o))).collect());
    }
    if log {
        static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
        let at = START.get_or_init(std::time::Instant::now).elapsed().as_secs_f32();
        eprintln!("[opt] {at:.1}s {m}{tag}: {} values, {} blocks, {} spill slots -> {} bytes ({main} main)", g.values.len(), al.order.len(), al.spill_slots, code.len());
    }
    if raw {
        let file = crate::jit::code_cache::dump_path(&format!("rava-opt-{entry:x}.bin"));
        // SAFETY: the code just installed (calls resolved).
        let _ = std::fs::write(&file, unsafe { std::slice::from_raw_parts(entry as *const u8, code.len()) });
        eprintln!("[opt-dump] {m} -> {file}");
    }
    if dumped {
        let file = crate::jit::code_cache::dump_path(&format!("rava-opt-{entry:x}.bin"));
        // SAFETY: the code just installed (calls resolved).
        let _ = std::fs::write(&file, unsafe { std::slice::from_raw_parts(entry as *const u8, code.len()) });
        // Where each value's code starts, for attributing samples to bytecode.
        let lines: Vec<String> = trace.iter().map(|(at, d)| format!("{at:x} {d}")).collect();
        let _ = std::fs::write(crate::jit::code_cache::dump_path(&format!("rava-opt-{entry:x}.map")), lines.join("
") + "
");
        eprintln!("[opt-dump] {m} -> {file}");
    }
    Some(CompiledMethod { entry, reg_entry: reg_entry.map_or(0, |r| entry + r), unresolved_deferrals: Default::default(), size: main, blocks: Vec::new(), frame_slots: extent as usize, tier: 2, optimized: true, deopts: Default::default() })
}

/// Perf map entries for `m`'s optimized code at `entry` (`size` bytes), one per run of code
/// generated for the same method (`trace`: where each value's code starts, described as
/// codegen does, ending with `@ holder.name:bci`), named after that method.
fn perf_map_by_leaf(entry: usize, size: usize, m: &'static Method, trace: &[(usize, String)]) {
    let mut runs: Vec<(usize, String)> = Vec::new();
    for (at, d) in trace {
        let leaf = d.rsplit_once(" @ ").and_then(|(_, w)| w.rsplit_once(':')).map_or(String::new(), |(n, _)| n.to_string());
        let leaf = if leaf.is_empty() { format!("{}.{}", m.holder.name, m.name) } else { leaf };
        if runs.last().is_none_or(|r| r.1 != leaf) {
            runs.push((*at, leaf));
        }
    }
    runs.sort_by_key(|r| r.0);
    let starts: Vec<usize> = runs.iter().map(|r| r.0).chain(std::iter::once(size)).collect();
    if runs.first().is_none_or(|r| r.0 > 0) {
        code_cache::perf_map(entry, starts[0], &format!("{}.{} [opt]", m.holder.name, m.name));
    }
    for (i, (at, leaf)) in runs.iter().enumerate() {
        if starts[i + 1] > *at {
            code_cache::perf_map(entry + at, starts[i + 1] - at, &format!("{leaf} [opt]"));
        }
    }
}
