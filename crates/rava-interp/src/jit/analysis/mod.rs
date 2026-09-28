//! Bytecode analyses: what the compiler is allowed to conclude about a method before it
//! emits anything. Every one of these is a pure function of the bytecode, the constant pool
//! and the descriptor -- never of a name, a workload or a past outcome.

pub(crate) mod localtypes;
pub(crate) mod normal_flow;
mod dp_reduce;
pub(crate) use dp_reduce::*;
mod dp_map;
pub(crate) use dp_map::*;
mod dp_scan;
pub(crate) use dp_scan::*;
mod foreach;
pub(crate) use foreach::*;
mod escape;
pub(crate) use escape::*;
mod licm;
mod nonnull;
pub(crate) use nonnull::*;
pub use licm::bce_census;
pub use licm::shape_census;
pub(crate) use licm::*;
mod trivial;
pub(crate) use trivial::*;
mod admission;
pub(crate) use admission::*;
mod hof;
pub(crate) use hof::*;
mod range;
pub(crate) use range::*;
mod confined_arrays;
pub(crate) use confined_arrays::*;
mod prep;
pub(crate) use prep::*;
mod dp_reduce2;
pub(crate) use dp_reduce2::*;
mod prim_arrays;
pub(crate) use prim_arrays::*;
