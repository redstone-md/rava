//! Parallel fork bodies: for each loop shape an analysis admitted, the worker function
//! the ring actually runs. One file per shape family.

mod reduce;
pub(crate) use reduce::*;
mod reduce2;
pub(crate) use reduce2::*;
mod map;
pub(crate) use map::*;
mod scan;
pub(crate) use scan::*;
