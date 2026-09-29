//! Baseline compiler (tier 1).
//!
//! Compiled code uses the interpreter's frame layout (`thread::header`): locals, header and
//! operand stack stay in the value-stack memory, so compiled and interpreted frames interleave
//! freely, exceptions and stack walks are shared, and any single instruction can be deferred to
//! the interpreter (`runtime::jit_interp_one`) with exact semantics.
//!
//! ABI (`EntryFn`): `rdi` = thread, `rsi` = locals, `rdx` = method; result in `rax`; an
//! exception is signalled through `Thread::pending`.

pub mod asm;
mod analysis;
mod code_cache;
mod compile;
mod emit;
mod emit_call;
mod emit_obj;
mod emit_reg;
mod emit_reg_mem;
mod fold;
mod inline;
mod inline_plan;
mod intrinsics;
mod opt;
mod profile;
mod stubs;
mod tier;
mod vstack;
mod runtime;
pub mod stats;

use crate::method::Method;
use crate::thread::{JResult, Thread};
use std::sync::atomic::Ordering;

pub type EntryFn = extern "sysv64" fn(*mut Thread, *mut u64, *const Method) -> u64;

/// A method's machine code and the native address of each block start.
pub struct CompiledMethod {
    pub entry: usize,
    /// Executions of sites compiled before their constants were resolved.
    pub unresolved_deferrals: std::sync::atomic::AtomicU32,
    pub size: usize,
    /// (bytecode index, native address) of every block leader, sorted by bci.
    pub blocks: Vec<(u32, usize)>,
    /// Value-stack slots the compiled frame uses (inlined frames included); an interpreted
    /// frame replaced by this code needs as much room.
    pub frame_slots: usize,
    /// 1 for the first compilation, 2 once recompiled hot (`tier.rs`).
    pub tier: u8,
}

impl CompiledMethod {
    pub(crate) fn address_of(&self, bci: u32) -> Option<usize> {
        self.blocks.binary_search_by_key(&bci, |b| b.0).ok().map(|i| self.blocks[i].1)
    }
}

/// Invocations before a method is compiled.
fn threshold() -> u32 {
    static T: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *T.get_or_init(|| std::env::var("RAVA_CORE_JIT_THRESHOLD").ok().and_then(|v| v.parse().ok()).unwrap_or(100))
}

pub fn enabled() -> bool {
    static E: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *E.get_or_init(|| std::env::var_os("RAVA_CORE_NO_JIT").is_none())
}

/// The initial `Method::entry`: run through the interpreter (which also dispatches natives).
pub(crate) fn interpreter_entry() -> usize {
    interpreter_adapter as *const () as usize
}

/// Compiled-code ABI entry that runs a method in the interpreter (or its native).
extern "sysv64" fn interpreter_adapter(t: *mut Thread, locals: *mut u64, m: *const Method) -> u64 {
    // SAFETY: generated code passes the current thread and a live method.
    let (t, m) = unsafe { (&mut *t, &*m) };
    match crate::interp::invoke(t, m, locals) {
        Ok(v) => v,
        Err(e) => {
            t.pending = e.0;
            0
        }
    }
}

/// Counts an interpreted invocation of `m`; compiles it once hot. Returns its compiled code.
pub(crate) fn on_invoke(m: &'static Method) -> Option<&'static CompiledMethod> {
    if let Some(cm) = m.compiled() {
        return Some(cm);
    }
    if !enabled() || m.is_native() || m.not_compilable.load(Ordering::Relaxed) {
        return None;
    }
    let n = m.invocations.load(Ordering::Relaxed) + 1;
    m.invocations.store(n, Ordering::Relaxed);
    if let Some(s) = stats::get() {
        s.interpreted_calls.fetch_add(1, Ordering::Relaxed);
    }
    if n < threshold() {
        return None;
    }
    compile_now(m)
}

/// Loop back-edges before a running interpreted method is replaced on the stack.
const OSR_THRESHOLD: u32 = 2000;

/// Counts a taken backward branch to `target`; once hot, returns the compiled address of
/// `target` for on-stack replacement.
pub(crate) fn on_backedge(m: &'static Method, target: usize) -> Option<usize> {
    if !enabled() || m.not_compilable.load(Ordering::Relaxed) {
        return None;
    }
    let n = m.backedges.load(Ordering::Relaxed) + 1;
    m.backedges.store(n, Ordering::Relaxed);
    if n < OSR_THRESHOLD {
        return None;
    }
    let cm = match m.compiled() {
        Some(cm) => cm,
        None => compile_now(m)?,
    };
    cm.address_of(target as u32)
}

/// Continues the interpreted frame at `locals` in compiled code at `address` until the method
/// returns (the frame header is already linked; the compiled epilogue unlinks it).
pub(crate) fn osr(t: &mut Thread, address: usize, locals: *mut u64) -> JResult<u64> {
    type Trampoline = extern "sysv64" fn(*mut Thread, *mut u64, usize) -> u64;
    // SAFETY: the trampoline is emitted below with exactly this ABI.
    let f: Trampoline = unsafe { std::mem::transmute(osr_trampoline()) };
    let r = f(t, locals, address);
    match t.take_pending() {
        Some(e) => Err(e),
        None => Ok(r),
    }
}

/// `(thread, locals, address)`: sets up the compiled-code registers and jumps into a body.
fn osr_trampoline() -> usize {
    static T: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *T.get_or_init(|| {
        use asm::R;
        let mut a = asm::Asm::default();
        compile::save_registers(&mut a);
        a.mov_rr(true, R::R12, R::Rdi);
        a.mov_rr(true, R::R13, R::Rsi);
        a.jmp_r(R::Rdx);
        code_cache::install(&a.finish())
    })
}

/// Deferrals of once-unresolved sites before the method is recompiled.
const RECOMPILE_AFTER: u32 = 64;
const MAX_RECOMPILES: u32 = 3;

pub(crate) fn note_unresolved_deferral(m: &'static Method) {
    let Some(cm) = m.compiled() else { return };
    if cm.unresolved_deferrals.fetch_add(1, Ordering::Relaxed) + 1 != RECOMPILE_AFTER {
        return;
    }
    if m.recompiles.fetch_add(1, Ordering::Relaxed) < MAX_RECOMPILES {
        if let Some(fresh) = compile::compile(m, cm.tier) {
            m.install(fresh);
        }
    }
}

fn compile_now(m: &'static Method) -> Option<&'static CompiledMethod> {
    let optimized = opt::everywhere().then(|| opt::compile(m)).flatten();
    match optimized.or_else(|| compile::compile(m, 1)) {
        Some(cm) => {
            if let Some(s) = stats::get() {
                s.compiled_methods.fetch_add(1, Ordering::Relaxed);
            }
            Some(m.install(cm))
        }
        None => {
            m.not_compilable.store(true, Ordering::Relaxed);
            None
        }
    }
}

/// Calls compiled code for `m` from Rust.
pub(crate) fn call(t: &mut Thread, m: &'static Method, cm: &CompiledMethod, locals: *mut u64) -> JResult<u64> {
    // SAFETY: `cm.entry` is code emitted for `m` with the `EntryFn` ABI.
    let f: EntryFn = unsafe { std::mem::transmute(cm.entry) };
    let r = f(t, locals, m);
    match t.take_pending() {
        Some(e) => Err(e),
        None => Ok(r),
    }
}
