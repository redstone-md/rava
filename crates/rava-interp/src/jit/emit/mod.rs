//! Code emission. One file per operation family; each takes a FunctionBuilder and the
//! compile Env and appends Cranelift IR, never deciding on its own whether it is allowed to.

pub mod invoke_site_census;

mod fields_prim;
pub(crate) use fields_prim::*;
mod fields_ref;
pub(crate) use fields_ref::*;
mod fields_arena;
pub(crate) use fields_arena::*;
mod fields_unchecked;
pub(crate) use fields_unchecked::*;
#[cfg(feature = "next-gen-gc")]
mod bitset_inline;
#[cfg(feature = "next-gen-gc")]
pub(crate) use bitset_inline::*;
mod arrays;
pub(crate) use arrays::*;
mod primitive_array_consumers;
pub(crate) use primitive_array_consumers::*;
#[cfg(feature = "next-gen-gc")]
mod primitive_arrays;
#[cfg(feature = "next-gen-gc")]
pub(crate) use primitive_arrays::*;
#[cfg(feature = "next-gen-gc")]
mod reference_arrays;
#[cfg(feature = "next-gen-gc")]
pub(crate) use reference_arrays::*;
mod licm;
pub(crate) use licm::*;
mod strings;
pub(crate) use strings::*;
mod calls;
pub(crate) use calls::*;
pub(crate) mod cap_thunk;
mod dispatch_pic;
pub(crate) use dispatch_pic::*;
pub(crate) mod pic_helpers;
pub(crate) use pic_helpers::*;
mod inline;
pub(crate) mod guarded_inline;
pub(crate) mod iterator_inline;

/// Write a primitive (int) field by id — the `$pos` advance in `iterator_inline`.
pub(crate) fn fields_prim_putfield(
    b: &mut cranelift::frontend::FunctionBuilder,
    env: &crate::jit::Env,
    ctx: cranelift::codegen::ir::Value,
    obj: cranelift::codegen::ir::Value,
    val_cell: cranelift::codegen::ir::Value,
    fid: i64,
) {
    fields_prim::native_putfield(b, env, ctx, obj, val_cell, fid, crate::heap::FK_INT as i64);
}

/// Read a primitive field by id — thin alias so `iterator_inline` need not import the module.
pub(crate) fn fields_prim_getfield(
    b: &mut cranelift::frontend::FunctionBuilder,
    env: &crate::jit::Env,
    ctx: cranelift::codegen::ir::Value,
    obj: cranelift::codegen::ir::Value,
    fid: i64,
) -> cranelift::codegen::ir::Value {
    fields_prim::native_getfield(b, env, ctx, obj, fid, crate::jit::JT::Int)
}
pub(crate) use inline::*;
pub(crate) mod collections;
pub(crate) use collections::*;
mod misc;
pub(crate) use misc::*;
mod intrinsics;
pub(crate) use intrinsics::*;
mod body_expr;
pub(crate) use body_expr::*;

mod body;
pub(crate) use body::*;
mod prologue;
pub(crate) use prologue::*;
pub(crate) mod ops;
mod lambda_mint;
pub(crate) use lambda_mint::*;
mod dispatch_table_probe;
pub(crate) use dispatch_table_probe::*;
/// Lane store-form census (`RAVA_LANE_STORE_CENSUS`, default OFF, diagnostic only).
pub(crate) mod store_census;
