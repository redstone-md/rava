//! What the compiler needs at run time rather than at emit time: the shared module, the
//! compile entry points, the inline-cache substrate, GC rooting and pool scanning.

mod module;
pub(crate) use module::*;
mod compile;
pub(crate) use compile::*;
mod ic;
pub(crate) use ic::*;
mod gcroots;
pub(crate) use gcroots::*;
/// PHASE A: Cranelift user stack maps, emitted beside the shadow frames and cross-checked
/// against them. Not re-exported flat — every reference is `runtime::stackmaps::…`, so the
/// opt-in machinery is visible at each call site.
#[cfg(feature = "next-gen-gc")]
pub(crate) mod stackmaps;
mod poolscan;
pub(crate) use poolscan::*;
mod imports;
pub(crate) use imports::*;
mod fork_plan;
pub(crate) use fork_plan::*;
pub(crate) mod finish;
pub(crate) use finish::*;
mod driver;
pub(crate) use driver::*;
