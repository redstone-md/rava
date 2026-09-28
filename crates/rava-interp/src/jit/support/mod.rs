//! Mechanics every emitter leans on: the operand stack, locals and cells, and the
//! arithmetic whose JVM semantics need explicit guards.

mod stack;
pub(crate) use stack::*;
mod locals;
pub(crate) use locals::*;
mod divrem;
pub(crate) use divrem::*;
