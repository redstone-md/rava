//! Receiver-type profiles for tier 2, read from tier-1 inline caches (per method and bytecode
//! index, as HotSpot's method data is): a call site whose cache only ever held one class is
//! inlined for that class behind an exact class check.

use super::runtime::IcCell;
use crate::klass::Klass;
use crate::method::Method;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

fn sites() -> &'static Mutex<HashMap<(usize, u32), &'static IcCell>> {
    static SITES: OnceLock<Mutex<HashMap<(usize, u32), &'static IcCell>>> = OnceLock::new();
    SITES.get_or_init(Default::default)
}

/// Remembers the inline cache of `m`'s call at `bci`.
pub(super) fn record(m: &'static Method, bci: usize, cell: &'static IcCell) {
    sites().lock().unwrap().insert((m as *const Method as usize, bci as u32), cell);
}

/// The only receiver class seen at `m`'s call at `bci`, if exactly one was.
pub(super) fn monomorphic(m: &'static Method, bci: usize) -> Option<&'static Klass> {
    let cell = *sites().lock().unwrap().get(&(m as *const Method as usize, bci as u32))?;
    // SAFETY: cached classes are immortal.
    cell.observed().map(|k| unsafe { &*(k as *const Klass) })
}
