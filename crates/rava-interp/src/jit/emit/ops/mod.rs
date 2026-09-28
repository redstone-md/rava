//! One file per opcode family, plus the context they share.

use crate::jit::*;

/// Everything one opcode emission needs. Built once per `emit_op` call; the families
/// reborrow their fields in a fixed preamble so no arm body had to be rewritten.
///
/// ⭐**THE PROCESS GATES THE FAMILIES READ ARE NOT IN HERE, AND THAT IS THE REMAINING COST ON
/// THIS PATH — RECORDED, NOT FIXED, BECAUSE THE TWO ENDS OF THE FIX LIVE ELSEWHERE.** Census of
/// this directory as it stands: `arena_inline_disabled()` alone is read at **20 sites**
/// (fields 5, invoke_dynamic 5, invoke_static 2, invoke_virtual 7, objects 1), and several arms
/// stack three or four more `OnceLock<bool>`s into ONE conjunction (`invoke_virtual`'s
/// `!arena_inline_disabled() && (!emit_invokeops_disabled() || (alist_size_inline_enabled() &&
/// …))` is three acquire loads to decide one branch). Every one is an acquire-ordered atomic
/// load of a value that cannot change during a run, and LLVM may neither hoist one out of the
/// surrounding emitter code nor merge two — so they are paid per EMITTED OPCODE, on a body the
/// COW/PROMO/FLAT unswitches emit up to three times, across the ~1500 bodies a boot compiles,
/// on the serial compile path.
///
/// The fold is the `jit::hot_gates()` shape: ONE word read at the natural entry, then bit tests.
/// It cannot be written from inside this file, because neither end of it is here — the word
/// would have to be read in `emit_blocks` (`emit/misc.rs`, once per BODY, not once per opcode),
/// threaded through `emit_op` (`jit/mod.rs`) into a new field on this struct, and consumed by
/// the family files (`fields.rs`, `invoke_*.rs`, `objects.rs`). Reading it in `emit_op` instead
/// would buy nothing: that runs per opcode too. Left as a hand-off deliberately, with the count
/// attached so the next round does not have to re-derive it.
///
/// Two raw `std::env::var_os` reads also survive in this directory — `RAVA_ALIST_DBG` in
/// `invoke_dynamic.rs` and `invoke_virtual.rs`, one per emitted ArrayList-get inline site. On
/// Windows MSVC that is `RtlQueryEnvironmentVariable` (UTF-16 encode of the key into a fresh
/// heap buffer, then a linear walk of the whole process environment block under a lock) to
/// answer a diagnostic whose value is fixed before the VM exists. Same hand-off; those files
/// are not this one.
pub(crate) struct Ops<'a, 'b> {
    pub(crate) b: &'a mut FunctionBuilder<'b>,
    pub(crate) stack: &'a mut Vec<SV>,
    pub(crate) block_shapes: &'a mut std::collections::HashMap<usize, Vec<JT>>,
    pub(crate) cur_open: &'a mut bool,
    pub(crate) vars: &'a [Variable],
    pub(crate) bc: &'a [u8],
    pub(crate) blocks: &'a std::collections::HashMap<usize, cranelift::prelude::Block>,
    pub(crate) pool: &'a ConstantPool,
    pub(crate) env: &'a Env<'a>,
    pub(crate) array_desc_cell: Value,
    pub(crate) locals_ptr: Value,
    pub(crate) cow: CowArm,
    #[cfg(feature = "next-gen-gc")]
    pub(crate) arena_proof: &'a [bool],
    #[cfg(feature = "next-gen-gc")]
    pub(crate) cursor_var: Option<Variable>,
    #[cfg(feature = "next-gen-gc")]
    pub(crate) licm: Option<Licm<'a>>,
    #[cfg(feature = "next-gen-gc")]
    pub(crate) dv: Option<DeepVars>,
}

mod consts;
pub(crate) use consts::consts;
pub(crate) mod fields;
pub(crate) use fields::fields;
mod objects;
pub(crate) use objects::objects;
/// The inline type test the `instanceof` arm of `objects` reaches for before its callback.
pub(crate) mod typecheck;
mod arrays;
pub(crate) use arrays::arrays;
mod arith;
pub(crate) use arith::arith;
mod stackops;
pub(crate) use stackops::stackops;
mod branch;
pub(crate) use branch::branch;
mod invoke_static;
pub(crate) use invoke_static::invoke_static;
mod invoke_virtual;
pub(crate) use invoke_virtual::invoke_virtual;
mod invoke_dynamic;
pub(crate) use invoke_dynamic::invoke_dynamic;
