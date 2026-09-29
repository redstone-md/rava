//! The Vm, split by what each group of methods is responsible for.
//!
//! Rust lets an inherent impl live in several modules of one crate, so every file here
//! contributes its own `impl Vm` block. Nothing about the type changed.

mod lifecycle;
pub(crate) mod thread_context;
#[cfg(all(test, feature = "next-gen-gc"))]
mod iterator_witness_tests;
#[cfg(all(test, feature = "next-gen-gc"))]
mod arena_string_contract_tests;
/// JVMS §5.5 initialisation state as the ACCESS path sees it (`Vm::ensure_init_fast`, the
/// per-site static binding). It was declared inside `vm/lifecycle.rs` behind a `#[path]`
/// attribute only because this file belonged to another work stream at the time; declared here
/// it is an ordinary sibling module and the attribute is gone.
mod clinit;
#[cfg(feature = "clinit-census")]
mod clinit_census;
pub(crate) mod threads;
mod drive;
pub(crate) mod aot_route_diag;
mod diag;
mod gc;
mod gc_census_report;
mod gc_census;
mod gc_collect;
#[cfg(feature = "next-gen-gc")]
mod gc_root_validation;
// These mixed AOT diagnostics depend on Windows-only `llvm_aot` probes, so keep the module gated.
#[cfg(all(windows, target_arch = "x86_64"))]
mod jit_probes;
pub(crate) mod probes;
mod jit_enter;
mod exec_kernel;
pub(crate) use exec_kernel::ExecKernel;
pub(crate) use jit_enter::hr_unwind_report; // [HR-UNWIND]: the `_NLG_Return2` row is memcpy, not an unwind — entries served by the constant-length cell zero
mod jit_plans;
pub(crate) mod jit_run;
mod jit_background;
mod jit_direct_resolve;
pub(crate) mod jit_direct_call;
#[cfg(feature = "next-gen-gc")]
pub(crate) use jit_direct_call::main_dt_learn as jit_direct_call_main_dt_learn;
mod jit_direct_invoke;
mod jit_natives;
mod jit_list_mutation;
pub(crate) mod fu_fastpath;
#[cfg(feature = "next-gen-gc")]
mod getter_fastpath;
#[cfg(feature = "next-gen-gc")]
mod pure_sam_fastpath;
#[cfg(feature = "next-gen-gc")]
mod choke_residue;
#[cfg(feature = "next-gen-gc")]
mod fu_miss_census;
mod natives_ws;
mod codec;
mod dispatch;
pub(crate) mod pic_fill;
mod selfreg;
mod structural_accessor;
mod jit_call;
mod jit_call_body;
mod pure_memo;
mod offload;
mod field_txn_offload;
mod lane_proof;
mod offload_drill;
mod fork_probe;
mod fork_iterator;
mod fork_apply;
mod fork_chunk;
mod fork_counted;
mod invoke;
// ADDED: the inliner's own refusal census (see inline_census.rs). Compile-time only.
pub(crate) mod inline_census;
mod inline_expansion_budget;
pub(crate) mod invoke_census;
mod lambda;
mod iterate;
pub(crate) use iterate::hr_compare_report;
mod apply;
mod native_backing;
mod strings;
mod arena_alloc;
mod arena_by_name;
mod layout;
pub(crate) mod inline_targets; // pub(crate): the dispatch-miss bridge calls its LEARN hook
pub(crate) mod lambda_prov; // static lambda provenance: the caller's own bytecode, not a profile
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub(crate) mod cnp_run;
#[cfg(all(windows, target_arch = "x86_64"))]
mod aot_llvm;
#[cfg(all(feature = "next-gen-gc", any(windows, target_os = "linux"), target_arch = "x86_64"))]
mod aot_link;
#[cfg(all(feature = "next-gen-gc", any(windows, target_os = "linux"), target_arch = "x86_64"))]
pub(crate) mod aot_arguments;
#[cfg(all(feature = "next-gen-gc", any(windows, target_os = "linux"), target_arch = "x86_64"))]
pub(crate) mod aot_cells;
#[cfg(all(feature = "next-gen-gc", any(windows, target_os = "linux"), target_arch = "x86_64"))]
pub(crate) mod aot_direct;
#[cfg(all(feature = "next-gen-gc", any(windows, target_os = "linux"), target_arch = "x86_64"))]
mod aot_direct_leaf;
#[cfg(all(feature = "next-gen-gc", any(windows, target_os = "linux"), target_arch = "x86_64"))]
pub(crate) mod aot_direct_cache;
mod statics;
mod jit_collections;
pub(crate) mod shared_memos;
pub(crate) use shared_memos::{RuntimeShared, SharedMemos};

/// **DEFAULT-ON**, `RAVA_NO_FAST_HANDOFF=1` disables, and so does `RAVA_NO_EMIT_MORE` so the
/// whole round bisects in one run.
///
/// One switch for every hand-off applied this round, all of which share one shape: **a question
/// answered from scratch on every execution whose answer is a constant of the class.**
///
///   * `Vm::ensure_init_handoff` — the §5.5 "has this class finished initialising" test, taken by
///     `getstatic_resolve` and the `new` path on every access instead of the one acquire load
///     `Vm::ensure_init_fast` decides it in.
///   * `Vm::apply_sam`'s SAM memo — the single abstract method of a receiver's class, re-resolved
///     per apply by up to five hierarchy walks (see `SAM_MEMO` in `vm/apply.rs`).
///
/// Off, every one of them runs the resolution it ran before, byte for byte: nothing is stored,
/// and no memo is read.
pub(crate) fn fast_handoff() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_FAST_HANDOFF").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// `RAVA_WORKER_HOT=1` — let the owning thread compile what workers found hot. Default OFF until
/// measured; see `Vm::note_worker_hot` for the population it targets.
///
/// ⭐**READ FROM `jit_run::vm_gates()`'s WORD ON THE ENTRY PATH, NOT FROM THIS STATIC.** Every
/// site that consults it consults `wide_tier_enabled()` in the same expression — two
/// acquire-ordered `OnceLock` loads to evaluate one `||` of two process constants — and the
/// entry-path site (`jit_run`'s worker arm, 482,902 visits a boot) now takes both from
/// `VG_WORKER_NOTE`, one bit of the word the prologue already holds. This accessor stays the
/// authority: the word is built by calling it, so an armed run behaves exactly as before, and
/// `RAVA_NO_VM_GATEWORD=1` sends the entry path back to calling it directly.
///
/// ⚠HAND-OFF, NOT MINE TO EDIT: `Vm::drain_worker_hot` (`vm/jit_background.rs:153`) opens with
/// the identical `worker_hot_enabled() || wide_tier_enabled()` pair, and it is called from
/// `jit_run` on every non-worker entry — so that gate pair is still two acquire loads on the full
/// ~21.7 M-passage population. Folding it means taking `vm_gates()`'s word as an argument there.
pub(crate) fn worker_hot_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_WORKER_HOT").is_some())
}

/// **DEFAULT-ON**: a GUEST class is compiled even when it descends from a native-backed one,
/// provided its own name is not in a natively-served namespace. `RAVA_NO_JIT_GUEST_SUB=1`
/// restores the old refusal. See `jit_native_served_deep_uncached` for why an ancestor is not
/// evidence about a class's own bytecode.
///
/// ⭐**THE POPULATION IT UNSTICKS IS NOT FLAT, WHICH IS WHY IT IS THE FIRST THING IN A LONG WHILE
/// TO MEASURE ABOVE THE FLOOR.** `RAVA_INTERP_WHO` on a datapack run: 1,818,007 interpreted
/// entries across 9,193 methods, and the top TEN are 46.9% of them — every one marked `absent`,
/// i.e. never compiled despite crossing the threshold hundreds of thousands of times
/// (`LinkedTreeMap.find` 115,227, `DoubleArrayList.equals` 150,917, guava's indexed iterator
/// 64,177). They were refused for their ANCESTOR, not for anything about themselves.
///
/// ```text
/// interpreted entries   1,816,630 -> 1,232,718   (-32%)
/// DataPackJson PARSE_MS     3,968 -> 3,216       (-19%)
/// process CPU, 6 ABBA pairs  24.33 -> 23.78 med  (-2.25%, min -2.85%, 5 of 6 pairs)
/// product boot          35.4/37.8 -> 35.0/37.4   (unchanged, inside its +-8% floor)
/// ```
///
/// 4 bit-exact gates green, 21 differential probes match `java -Xint`, DataPackJson hash
/// unchanged — which is the evidence the flag existed to collect.
pub(crate) fn guest_sub_jit_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_JIT_GUEST_SUB").is_none())
}
