//! Compiled-method handles and their status, plus the kernel profile a caller reads back.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::jit::*;

/// Per-method JIT state, cached on the Vm keyed by the `Code`'s address.
pub enum JitStatus {
    /// Seen `n` times; still interpreted until the hotness threshold.
    Cold(u32),
    /// A persistent per-VM compiler owns a pending native-code job. Calls keep
    /// interpreting until the main VM thread publishes its completed result.
    Compiling,
    /// Attempted and not compilable (unsupported opcodes) — always interpret.
    Failed,
    /// Native code ready.
    Compiled(JitFn),
}

/// A compiled method. Cranelift retains finalized code until explicit free_memory
/// (unused here), even after the thread-local JITModule drops. This handle does
/// not own that allocation. Metadata embedded in native code must survive raw
/// pointers retained outside this handle, including superseded cache entries.
pub struct JitFn {
    pub(crate) ptr: *const u8,
    pub ret: Option<JT>,
    /// Deep-IC v3: the body was emitted with the SELF-REGISTERING prologue
    /// (fault check, non-arg zeroing, inline cell-root push, box discipline) —
    /// the machine-code dispatch IC may call it directly on the deep arm with
    /// no caller-side registration. False for leaf, folded, parallel and
    /// gate-off bodies.
    pub(crate) self_reg: bool,
    /// register-ABI arc (I1a): Some when this entry was compiled with the register
    /// signature `fn(array_desc_cell, prim_args...) -> prim_ret` via `compile_register`.
    /// None ⇒ the default memory-locals ABI (`fn(*i64 locals, i64) -> i64`), invoked by
    /// `call`/`call_desc`. A register entry must be invoked through its raw `ptr` with the
    /// matching primitive signature.
    pub(crate) reg: Option<RegAbi>,
    /// Interned printable-identity id (`register_compiled_method`), stamped at the
    /// `jit_cache` publication point where the class/name/descriptor are in hand. Carried
    /// here so the entry boundary publishes the current method with no map lookup and no
    /// string work. 0 until published.
    pub(crate) mid: u32,
    /// Finalized machine-code length in bytes (`CompiledCode::code_info().total_size`), so
    /// `pc_profile` can map a sampled instruction pointer back to this method. 0 when the
    /// size was unavailable. Carried on the entry rather than in a side table because the
    /// pointer and the size are only both in hand at the compile site.
    pub(crate) code_len: u32,
}

/// Sendable representation of a finalized entry. The code pointer crosses the
/// compiler channel as an integer; only the owning VM thread reconstructs the
/// `JitFn` and publishes it while the persistent compiler still owns its module.
pub(crate) struct JitFnParts {
    pub ptr: usize,
    pub ret: Option<JT>,
    pub self_reg: bool,
    pub reg: Option<RegAbi>,
    pub code_len: u32,
}

impl JitFn {
    pub(crate) fn into_parts(self) -> JitFnParts {
        JitFnParts {
            ptr: self.ptr as usize,
            ret: self.ret,
            self_reg: self.self_reg,
            reg: self.reg,
            code_len: self.code_len,
        }
    }

    pub(crate) fn from_parts(parts: JitFnParts) -> Self {
        Self {
            ptr: parts.ptr as *const u8,
            ret: parts.ret,
            self_reg: parts.self_reg,
            reg: parts.reg,
            mid: 0,
            code_len: parts.code_len,
        }
    }

    /// Call the compiled function. `locals` must have length >= max_locals; the
    /// returned i64 holds the result bits (Int sign-extended, Float/Double the
    /// IEEE bits, Long as-is). Safety: `ptr` is a finalized Cranelift function
    /// with the `extern "C" fn(*mut i64) -> i64` signature we defined.
    pub unsafe fn call(&self, locals: &mut [i64]) -> i64 {
        self.call_desc(locals, 0)
    }

    /// Call with the array descriptor-base cell pointer (second ABI param) for
    /// native array loads/length. Pass 0 when the method touches no arrays.
    pub unsafe fn call_desc(&self, locals: &mut [i64], desc_cell: i64) -> i64 {
        let f: extern "C" fn(*mut i64, i64) -> i64 = std::mem::transmute(self.ptr);
        f(locals.as_mut_ptr(), desc_cell)
    }

    /// Raw code pointer, for calling after the cache borrow is released.
    pub fn raw(&self) -> *const u8 {
        self.ptr
    }
}

/// Discover basic-block leaders (branch targets + the instruction after each
/// branch). `None` if any unsupported/excluded opcode is present.
/// 19.1 M4 kernel-scan profile: a JIT-pass bytecode analysis classifying whether a method body is a
/// PURE primitive-array compute loop — the shape a 15.1-style native kernel can absorb. Counts allocs
/// (`new`/`*newarray`), non-`java/lang/Math` invokes (any callback = disqualifies), the `java/lang/Math`
/// intrinsic calls (allowed), double/float array loads+stores (the kernel's primitive traffic), int/long
/// array traffic (context), float/double arithmetic (math density), and whether the body loops (backward
/// branch). Read-only — walks the bytecode with the JIT's own `insn_len_at`; no compilation, no effect.
#[derive(Clone, Copy, Default)]
pub struct KernelProfile {
    pub allocs: u32,
    pub nonmath_calls: u32,
    pub math_calls: u32,
    pub dfloat_array_ops: u32,
    pub int_array_ops: u32,
    pub fp_arith: u32,
    pub has_loop: bool,
    pub code_len: usize,
}

// 19.3: retained dormant (hot-path hook unwired at the release seal; re-wire for future recon).
