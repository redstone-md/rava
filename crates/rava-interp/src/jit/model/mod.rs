//! The data the compiler passes between its stages. No behaviour beyond the accessors a
//! type owes its own invariants.

mod loops;
pub(crate) use loops::*;
mod values;
pub(crate) use values::*;
mod plans;
pub(crate) use plans::*;
mod handles;
pub(crate) use handles::*;
