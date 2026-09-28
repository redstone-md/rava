//! Baseline JIT (Cranelift).
//!
//! Goal: compile hot JVM methods to native code so runtime speed can beat the
//! JVM. Stage 1 (this file) is the NUMERIC CORE: it compiles methods whose body
//! is int/long/float/double arithmetic, locals, branches and returns — no heap,
//! no calls, no exception handlers. Such methods (e.g. noise-function inner
//! math) are the chunk-gen hot path. Anything outside the supported opcode set
//! makes `compile` return `None`, and the caller falls back to the interpreter.
//!
//! Representation: every local slot is a uniform i64 cell (floats/doubles stored
//! by bit-cast). The compile-time operand stack tracks (cranelift value, type)
//! so each opcode emits the correctly-typed instruction. The operand stack is
//! required to be empty at every branch target (true for javac-compiled control
//! flow); locals flow across blocks as Cranelift `Variable`s (SSA handled by the
//! builder). Integer div/rem use a VM callback for JVM exception/overflow
//! semantics; long div/rem and float rem still fall back.






















































use emit::ops::{Ops, arith, arrays, branch, consts, fields, invoke_dynamic, invoke_static, invoke_virtual, objects, stackops};
pub(crate) mod compile_bail;
/// WHAT THE TWO BAKED RANGE TESTS IN FRONT OF COMPILED CALLS DISCRIMINATE
/// (`[CALLSEQ]`, default off): pool membership plus branchless diagnostics for
/// descriptor class. Generated native hits use one rooted arm.
pub(crate) mod callseq;
mod direct;
pub(crate) use direct::*;

mod cow;
pub(crate) use cow::*;

mod env;
pub(crate) mod ir_budget;
pub(crate) mod compile_phase_trace;
#[cfg(test)]
mod inline_local_contract_tests;
pub(crate) use env::*;

mod support;
pub(crate) use support::*;

pub(crate) mod runtime;
pub(crate) use runtime::*;

use crate::value::{BOX_TAG};
mod model;
pub(crate) use model::*;

pub(crate) mod emit;
pub(crate) use emit::*;

mod parallel;
pub(crate) use parallel::*;

mod analysis;
pub use analysis::bce_census;
pub use analysis::shape_census;
pub(crate) use analysis::*;

pub(crate) mod callbacks;
pub(crate) use callbacks::*;

pub(crate) mod freeze_resolve;
pub(crate) mod dispatch_table;
#[cfg(feature = "next-gen-gc")]
pub(crate) mod dispatch_diag;
pub(crate) mod lambda_shape;
pub(crate) mod trivial_getter;
pub(crate) mod structural_accessor;
pub(crate) use dispatch_table::take_dispatch_site;

mod resolvelock;
pub(crate) use resolvelock::*;

mod vmref;
pub(crate) use vmref::*;

mod counters;
pub(crate) use counters::*;

mod gates;
pub(crate) use gates::*;

mod abi;
pub(crate) use abi::*;

/// Pure bytecode helpers, split out of this file (see `bytecode.rs`). Re-exported so no
/// call site outside this module changed.
pub(crate) mod bytecode;
pub(crate) use bytecode::*;


use cranelift::codegen::ir::immediates::Offset32;
use cranelift::prelude::types::{F32, F64, I32, I64, I8};
use cranelift::prelude::*;
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{Linkage, Module};
use rava_classfile::{Code, Constant, ConstantPool};
#[cfg(feature = "next-gen-gc")]
use std::sync::atomic::{AtomicU64, Ordering};



/// TRACER (next-gen-gc): dual-path constant-slot getfield for a REFERENCE field.
/// Arena cells store refs in the low-2-bit scheme, so the arena arm RE-TAGS the
/// loaded cell into the JIT high-word operand form before pushing:
///   `0b00` raw ArenaPtr / null → as-is (already a valid Obj/null operand cell)
///   `0b01` Ref `(h<<2)|1` → `h | REF_TAG`        `0b10` Array → `h | ARRAY_TAG`
///   `0b11` boxed Str/sentinel → cold callback (needs the jit_box_table).
/// Heap-handle / null receivers take the unchanged high-word path.
/// WHICH EMITTER CREATES THE BLOCKS (`RAVA_JIT_BLOCK_CENSUS`, default OFF).
///
/// The ring's hottest method is 852 blocks for four lines of Java, 62.7% of them doing no memory
/// work and no call, and 81.5% of THOSE bare merges whose phi webs are the plausible source of its
/// 2555 movq. Folding the arena ref-field classify reached 16 of 435 merges and folding its flat
/// twin reached zero more, so those emitters are NOT the source - two wrong guesses in two turns.
///
/// A static count of `create_block()` per function says `emit_dispatch_pic` builds TWENTY blocks
/// per invocation, against 7 for the ref-field emitters, and the same method has 103 call sites of
/// which ~65% emit a PIC. That is a strong inference and it is still an inference. This counts the
/// invocations for real, so the next person acts on a measurement.
#[cfg(feature = "next-gen-gc")]
pub(crate) mod block_census {
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
    pub(crate) static PIC: AtomicU64 = AtomicU64::new(0);
    pub(crate) static REF_FIELD: AtomicU64 = AtomicU64::new(0);
    pub(crate) static PUTFIELD: AtomicU64 = AtomicU64::new(0);
    /// Blocks ACTUALLY created, from `dfg.num_blocks()` snapshots around the emitter. The static
    /// `create_block()` count is an upper bound - most of `emit_dispatch_pic`'s twenty sites are
    /// behind flags that are off in a default build - so multiplying invocations by it overstates
    /// the total, which is a mistake this census previously shipped.
    pub(crate) static PIC_BLOCKS: AtomicU64 = AtomicU64::new(0);
    /// Raw Cranelift IR instructions emitted inside dispatch PICs, before backend optimization.
    pub(crate) static PIC_CLIF_INSTS: AtomicU64 = AtomicU64::new(0);
    /// Raw Cranelift IR instructions in compiled functions, before backend optimization.
    pub(crate) static TOTAL_CLIF_INSTS: AtomicU64 = AtomicU64::new(0);
    pub(crate) static CENSUSED_METHODS: AtomicU64 = AtomicU64::new(0);
    /// Dispatch sites whose RECEIVER carries the method-entry unswitch's arena proof (`SV::arena`).
    /// At such a site `emit_is_arena` is statically true, so the heap-Ref arm - measured at 6 of the
    /// 12 blocks a site emits - is unreachable and need not be emitted. This counter answers the only
    /// question that decides whether that is a lever or a curiosity: HOW MANY sites qualify. The
    /// proof holds solely inside the unswitch's arena body for values aloaded from the ONE pinned
    /// slot (plus inlined-callee arg binding), so low coverage is the expected outcome, not a bug.
    pub(crate) static PIC_ARENA_RECV: AtomicU64 = AtomicU64::new(0);
    pub(crate) fn add_blocks(c: &AtomicU64, n: u64) {
        if enabled() {
            c.fetch_add(n, Relaxed);
        }
    }
    pub(crate) fn add_clif_insts(n: u64) {
        if enabled() {
            PIC_CLIF_INSTS.fetch_add(n, Relaxed);
        }
    }

    /// Publish cumulative IR counts periodically so the boot needs no method-name filter.
    pub(crate) fn note_method_clif_insts(n: u64) {
        if !enabled() {
            return;
        }
        let total = TOTAL_CLIF_INSTS.fetch_add(n, Relaxed) + n;
        let methods = CENSUSED_METHODS.fetch_add(1, Relaxed) + 1;
        if methods % 1000 == 0 {
            let pic = PIC_CLIF_INSTS.load(Relaxed);
            eprintln!(
                "[JIT-IR-CENSUS] methods={methods} total={total} pic={pic} ({:.1}%)",
                if total == 0 { 0.0 } else { 100.0 * pic as f64 / total as f64 }
            );
        }
    }

    /// `RAVA_JIT_BLOCK_CENSUS`, now bit `HG_BLOCK_CENSUS` of the JIT gate word instead of a
    /// private `OnceLock`. `emit_dispatch_pic` calls `note`/`add_blocks` FOUR times per emitted
    /// site (~3000 sites a boot), and `emit` calls the same accessor from two more files: four
    /// separate acquire loads of four different statics per site become four bit tests of one
    /// word the emitter has usually already loaded for another gate. Small population — this is
    /// housekeeping that deletes a static, not a lever.
    pub(crate) fn enabled() -> bool {
        super::hot_gates() & super::hg_bit(super::HG_BLOCK_CENSUS) != 0
    }
    pub(crate) fn note(c: &AtomicU64) {
        if enabled() {
            c.fetch_add(1, Relaxed);
        }
    }
    /// ⚠ The multipliers below are STATIC `create_block()` counts and therefore an UPPER BOUND,
    /// not the default-configuration figure. Several `emit_dispatch_pic` blocks are
    /// flag-conditional: gprobe/wprobe/pmerge exist only under IC_PARALLEL (default OFF),
    /// tprobe/tbounds/tload/decide only under RAVA_DISPATCH_TABLE (default OFF), sblk/cont only in
    /// heap_mode 1, and refpic/refhit/refcheck only in heap_mode 2. The unified rooted hit removed
    /// the arena and heap leaf/deep split blocks. The INVOCATION counts are exact; treat the
    /// products as "which emitter dominates", never as a block total.
    pub(crate) fn report() -> String {
        if !enabled() {
            return String::new();
        }
        let (p, r, w) = (PIC.load(Relaxed), REF_FIELD.load(Relaxed), PUTFIELD.load(Relaxed));
        let pb = PIC_BLOCKS.load(Relaxed);
        let per = if p > 0 { pb as f64 / p as f64 } else { 0.0 };
        let ar = PIC_ARENA_RECV.load(Relaxed);
        let arpct = if p > 0 { 100.0 * ar as f64 / p as f64 } else { 0.0 };
        let pi = PIC_CLIF_INSTS.load(Relaxed);
        let ti = TOTAL_CLIF_INSTS.load(Relaxed);
        let ipct = if ti == 0 { 0.0 } else { 100.0 * pi as f64 / ti as f64 };
        format!(
            "emit_dispatch_pic={p} calls, {pb} blocks REALLY created ({per:.1}/call) | \
             PIC_CLIF_INSTS={pi}/{ti} ({ipct:.1}%) | arena-PROVEN recv={ar} ({arpct:.1}%) | \
             arena ref getfield={r} | ref putfield={w}"
        )
}

}
fn emit_op(
    b: &mut FunctionBuilder,
    vars: &[Variable],
    stack: &mut Vec<SV>,
    bc: &[u8],
    pc: usize,
    op: u8,
    blocks: &std::collections::HashMap<usize, cranelift::prelude::Block>,
    block_shapes: &mut std::collections::HashMap<usize, Vec<JT>>,
    cur_open: &mut bool,
    pool: &ConstantPool,
    env: &Env,
    array_desc_cell: Value,
    locals_ptr: Value,
    // What the CURRENT body knows about the M3 COW arm flag for the whole
    // invocation (see the method-entry COW UNSWITCH in `build_body`).
    cow: CowArm,
    // Which of THIS body's local slots the method-entry unswitch proved arena (see
    // `flat_unswitch_slot`); an absent/false entry leaves the site's own guard in place.
    #[cfg(feature = "next-gen-gc")] arena_proof: &[bool],
    #[cfg(feature = "next-gen-gc")] cursor_var: Option<Variable>,
    #[cfg(feature = "next-gen-gc")] licm: Option<Licm>,
    #[cfg(feature = "next-gen-gc")] dv: Option<DeepVars>,
) -> Option<()> {
    // Includes nested inline emission. Refusal propagates to the whole function;
    // partially emitted IR must never be finalized or installed.
    if !ir_budget::allows(b.func) {
        return None;
    }
    let mut o = Ops {
        b,
        stack,
        block_shapes,
        cur_open,
        vars,
        bc,
        blocks,
        pool,
        env,
        array_desc_cell,
        locals_ptr,
        cow,
        #[cfg(feature = "next-gen-gc")]
        arena_proof,
        #[cfg(feature = "next-gen-gc")]
        cursor_var,
        #[cfg(feature = "next-gen-gc")]
        licm,
        #[cfg(feature = "next-gen-gc")]
        dv,
    };
    match op {
        0x00 | 0x01 | 0x19 | 0x2a..=0x2d | 0x3a | 0x4b..=0x4e | 0x12 | 0x13 | 0x14 | 0x02..=0x08 | 0x09 | 0x0a | 0x0b..=0x0d | 0x0e | 0x0f | 0x10 | 0x11 | 0x15 | 0x16 | 0x17 | 0x18 | 0x1a..=0x1d | 0x1e..=0x21 | 0x22..=0x25 | 0x26..=0x29 | 0x36..=0x39 | 0x3b..=0x3e | 0x3f..=0x42 | 0x43..=0x46 | 0x47..=0x4a => consts(&mut o, pc, op)?,
        0xb4 | 0xb5 | 0xb2 | 0xb3 => fields(&mut o, pc, op)?,
        0xc0 | 0xc1 | 0xbb | 0xbc | 0xbd => objects(&mut o, pc, op)?,
        0x2e | 0x33 | 0x34 | 0x35 | 0x2f | 0x30 | 0x31 | 0x32 | 0x4f | 0x50 | 0x51 | 0x52 | 0x54 | 0x55 | 0x56 | 0x53 | 0xbe => arrays(&mut o, pc, op)?,
        0x60 | 0x64 | 0x68 | 0x6c | 0x70 | 0x74 | 0x78 | 0x7a | 0x7c | 0x7e | 0x80 | 0x82 | 0x61 | 0x65 | 0x69 | 0x6d | 0x71 | 0x75 | 0x79 | 0x7b | 0x7d | 0x7f | 0x81 | 0x83 | 0x62 | 0x66 | 0x6a | 0x6e | 0x72 | 0x76 | 0x63 | 0x67 | 0x6b | 0x6f | 0x73 | 0x77 | 0x85 | 0x86 | 0x87 | 0x88 | 0x89 | 0x8a | 0x8b | 0x8c | 0x8d | 0x8e | 0x8f | 0x90 | 0x91 | 0x92 | 0x93 | 0x94 | 0x95..=0x98 => arith(&mut o, pc, op)?,
        0x57 | 0x58 | 0x59 | 0x5a | 0x5c | 0x5f | 0x84 => stackops(&mut o, pc, op)?,
        0x99..=0x9e | 0x9f..=0xa4 | 0xa5 | 0xa6 | 0xa7 | 0xaa | 0xab | 0xc6 | 0xc7 | 0xac | 0xad | 0xae | 0xaf | 0xb0 | 0xb1 => branch(&mut o, pc, op)?,
        0xbf | 0xb8 | 0xb7 => invoke_static(&mut o, pc, op)?,
        0xb6 => invoke_virtual(&mut o, pc, op)?,
        0xba | 0xb9 => invoke_dynamic(&mut o, pc, op)?,
        _ => return None,
    }
    Some(())
}

#[cfg(test)]
mod tests;

thread_local! {
    /// One JIT module reused across every compiled method on this thread. Each
    /// `JITModule` reserves its own executable-memory regions, so a module per
    /// method (the previous design) cost ~MB each and ballooned RSS to ~14GB on a
    /// full RAVA_JIT=1 boot. Packing all methods into one shared module drops the
    /// per-method cost to roughly the code size (~KB). The module is taken out for
    /// a compile and restored by `ModuleGuard` on every exit path, so an emit bail
    /// (the common `?` return) preserves the accumulated code.
    static SHARED_MODULE: std::cell::RefCell<Option<JITModule>> =
        const { std::cell::RefCell::new(None) };
}
